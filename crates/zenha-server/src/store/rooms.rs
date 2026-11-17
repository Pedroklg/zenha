//! Rooms: creation, editing, ordering, hierarchy, removal and identifier generation.

use rusqlite::{Connection, TransactionBehavior, params};
use zenha_protocol::{AdminRoom, MAX_ROOM_NAME_LEN, MAX_ROOMS_COUNT};

use super::{ChatStore, ChatStoreError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateRoomOutcome {
    Created(AdminRoom),
    LimitReached,
    InvalidParent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateRoomOutcome {
    Updated(AdminRoom),
    NotFound,
    InvalidParent,
}

/// Changes to a room; `None` keeps the field and `Some(None)` clears the password or parent room.
#[derive(Default)]
pub struct RoomUpdate {
    pub title: Option<String>,
    pub position: Option<usize>,
    pub password_hash: Option<Option<String>>,
    pub parent: Option<Option<String>>,
}

#[derive(Clone)]
pub struct RoomRecord {
    pub id: String,
    pub title: String,
    pub position: usize,
    pub parent: Option<String>,
    pub password_hash: Option<String>,
}

impl RoomRecord {
    pub fn admin(&self) -> AdminRoom {
        AdminRoom {
            id: self.id.clone(),
            title: self.title.clone(),
            position: self.position,
            locked: self.password_hash.is_some(),
            parent: self.parent.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteRoomCheck {
    Ready,
    NotFound,
    LastRoom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteRoomOutcome {
    Deleted,
    NotFound,
    LastRoom,
}

impl ChatStore {
    pub fn seed_rooms_if_empty_sync(&self, rooms: &[String]) -> Result<(), ChatStoreError> {
        let mut guard = self.conn.lock().map_err(|_| ChatStoreError::LockPoisoned)?;
        seed_rooms_sync(&mut guard, rooms)
    }

    #[cfg(test)]
    pub async fn list_rooms(&self) -> Result<Vec<AdminRoom>, ChatStoreError> {
        let records = self.room_records().await?;
        Ok(records.iter().map(RoomRecord::admin).collect())
    }

    pub async fn room_records(&self) -> Result<Vec<RoomRecord>, ChatStoreError> {
        self.with_conn(|conn| load_room_records(conn)).await
    }

    pub async fn create_room(
        &self,
        title: String,
        password_hash: Option<String>,
        parent: Option<String>,
    ) -> Result<CreateRoomOutcome, ChatStoreError> {
        self.with_conn(move |conn| create_room_sync(conn, &title, password_hash, parent))
            .await
    }

    pub async fn update_room(
        &self,
        id: String,
        update: RoomUpdate,
    ) -> Result<UpdateRoomOutcome, ChatStoreError> {
        self.with_conn(move |conn| update_room_sync(conn, &id, update))
            .await
    }

    pub async fn check_can_delete_room(
        &self,
        id: String,
    ) -> Result<DeleteRoomCheck, ChatStoreError> {
        self.with_conn(move |conn| check_can_delete_room_sync(conn, &id))
            .await
    }

    pub async fn commit_delete_room(
        &self,
        id: String,
    ) -> Result<DeleteRoomOutcome, ChatStoreError> {
        self.with_conn(move |conn| commit_delete_room_sync(conn, &id))
            .await
    }
}

fn seed_rooms_sync(conn: &mut Connection, rooms: &[String]) -> Result<(), ChatStoreError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let count: i64 = tx.query_row("SELECT COUNT(*) FROM rooms", [], |row| row.get(0))?;
    if count == 0 {
        for (idx, id) in rooms.iter().enumerate() {
            let pos = i64::try_from(idx).unwrap_or(i64::MAX);
            tx.execute(
                "INSERT INTO rooms (id, title, position) VALUES (?1, ?2, ?3)",
                params![id, id, pos],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn load_room_records(conn: &Connection) -> Result<Vec<RoomRecord>, ChatStoreError> {
    let mut stmt = conn.prepare(
        "SELECT id, title, parent, password_hash FROM rooms ORDER BY position ASC, id ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
    })?;
    let mut out = Vec::new();
    for (position, row) in rows.enumerate() {
        let (id, title, parent, password_hash) = row?;
        out.push(RoomRecord {
            id,
            title,
            position,
            parent,
            password_hash,
        });
    }
    Ok(out)
}

/// A room can only have another top-level room as parent, and a room with children does not become a child.
fn parent_is_valid(rooms: &[RoomRecord], id: &str, parent: &str) -> bool {
    parent != id
        && rooms.iter().any(|r| r.id == parent && r.parent.is_none())
        && !rooms.iter().any(|r| r.parent.as_deref() == Some(id))
}

fn create_room_sync(
    conn: &mut Connection,
    title: &str,
    password_hash: Option<String>,
    parent: Option<String>,
) -> Result<CreateRoomOutcome, ChatStoreError> {
    let trimmed = title.trim();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing = load_room_records(&tx)?;
    if existing.len() >= MAX_ROOMS_COUNT {
        return Ok(CreateRoomOutcome::LimitReached);
    }
    let existing_ids: Vec<String> = existing.iter().map(|r| r.id.clone()).collect();
    let id = unique_room_id(trimmed, &existing_ids);
    if parent
        .as_deref()
        .is_some_and(|p| !parent_is_valid(&existing, &id, p))
    {
        return Ok(CreateRoomOutcome::InvalidParent);
    }
    let position = existing.len();
    let pos_i64 = i64::try_from(position).unwrap_or(i64::MAX);
    tx.execute(
        "INSERT INTO rooms (id, title, position, parent, password_hash)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, trimmed, pos_i64, parent, password_hash],
    )?;
    tx.commit()?;
    Ok(CreateRoomOutcome::Created(
        RoomRecord {
            id,
            title: trimmed.to_owned(),
            position,
            parent,
            password_hash,
        }
        .admin(),
    ))
}

fn update_room_sync(
    conn: &mut Connection,
    id: &str,
    update: RoomUpdate,
) -> Result<UpdateRoomOutcome, ChatStoreError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut rooms = load_room_records(&tx)?;
    let Some(mut idx) = rooms.iter().position(|r| r.id == id) else {
        return Ok(UpdateRoomOutcome::NotFound);
    };
    if let Some(Some(parent)) = &update.parent
        && !parent_is_valid(&rooms, id, parent)
    {
        return Ok(UpdateRoomOutcome::InvalidParent);
    }
    let room = &mut rooms[idx];
    if let Some(new_title) = update.title {
        room.title = new_title.trim().to_owned();
    }
    if let Some(parent) = update.parent {
        room.parent = parent;
    }
    if let Some(hash) = update.password_hash {
        room.password_hash = hash;
    }
    if let Some(target) = update.position {
        let clamped = target.min(rooms.len().saturating_sub(1));
        if clamped != idx {
            let item = rooms.remove(idx);
            rooms.insert(clamped, item);
            idx = clamped;
        }
    }
    for (pos, room) in rooms.iter_mut().enumerate() {
        room.position = pos;
        let pos_i64 = i64::try_from(pos).unwrap_or(i64::MAX);
        tx.execute(
            "UPDATE rooms SET title = ?1, position = ?2, parent = ?3, password_hash = ?4
             WHERE id = ?5",
            params![
                room.title,
                pos_i64,
                room.parent,
                room.password_hash,
                room.id
            ],
        )?;
    }
    tx.commit()?;
    Ok(UpdateRoomOutcome::Updated(rooms[idx].admin()))
}

fn check_can_delete_room_sync(
    conn: &mut Connection,
    id: &str,
) -> Result<DeleteRoomCheck, ChatStoreError> {
    let tx = conn.transaction()?;
    let rooms = load_ordered_rooms(&tx)?;
    if !rooms.iter().any(|(room_id, _)| room_id == id) {
        return Ok(DeleteRoomCheck::NotFound);
    }
    if rooms.len() <= 1 {
        return Ok(DeleteRoomCheck::LastRoom);
    }
    Ok(DeleteRoomCheck::Ready)
}

fn commit_delete_room_sync(
    conn: &mut Connection,
    id: &str,
) -> Result<DeleteRoomOutcome, ChatStoreError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let rooms = load_ordered_rooms(&tx)?;
    if !rooms.iter().any(|(room_id, _)| room_id == id) {
        return Ok(DeleteRoomOutcome::NotFound);
    }
    if rooms.len() <= 1 {
        return Ok(DeleteRoomOutcome::LastRoom);
    }
    tx.execute("DELETE FROM rooms WHERE id = ?1", params![id])?;
    tx.execute(
        "UPDATE rooms SET parent = NULL WHERE parent = ?1",
        params![id],
    )?;
    tx.execute("DELETE FROM chat_messages WHERE room = ?1", params![id])?;
    super::webhooks::delete_room_webhooks(&tx, id)?;
    let remaining = rooms.into_iter().filter(|(room_id, _)| room_id != id);
    for (pos, (room_id, _)) in remaining.enumerate() {
        let pos_i64 = i64::try_from(pos).unwrap_or(i64::MAX);
        tx.execute(
            "UPDATE rooms SET position = ?1 WHERE id = ?2",
            params![pos_i64, room_id],
        )?;
    }
    tx.commit()?;
    Ok(DeleteRoomOutcome::Deleted)
}

fn load_ordered_rooms(
    tx: &rusqlite::Transaction<'_>,
) -> Result<Vec<(String, String)>, ChatStoreError> {
    let mut stmt = tx.prepare("SELECT id, title FROM rooms ORDER BY position ASC, id ASC")?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn fold_latin(c: char) -> char {
    match c {
        'á' | 'à' | 'â' | 'ã' | 'ä' | 'å' | 'Á' | 'À' | 'Â' | 'Ã' | 'Ä' | 'Å' => 'a',
        'é' | 'è' | 'ê' | 'ë' | 'É' | 'È' | 'Ê' | 'Ë' => 'e',
        'í' | 'ì' | 'î' | 'ï' | 'Í' | 'Ì' | 'Î' | 'Ï' => 'i',
        'ó' | 'ò' | 'ô' | 'õ' | 'ö' | 'ø' | 'Ó' | 'Ò' | 'Ô' | 'Õ' | 'Ö' | 'Ø' => 'o',
        'ú' | 'ù' | 'û' | 'ü' | 'Ú' | 'Ù' | 'Û' | 'Ü' => 'u',
        'ç' | 'Ç' => 'c',
        'ñ' | 'Ñ' => 'n',
        'ý' | 'ÿ' | 'Ý' => 'y',
        other => other.to_ascii_lowercase(),
    }
}

pub fn slugify_room_title(title: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in title.trim().chars() {
        let mapped = fold_latin(ch);
        if mapped.is_ascii_lowercase() || mapped.is_ascii_digit() || mapped == '_' {
            out.push(mapped);
            last_dash = false;
        } else if !out.is_empty() && !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.len() > MAX_ROOM_NAME_LEN {
        out.truncate(MAX_ROOM_NAME_LEN);
        while out.ends_with('-') {
            out.pop();
        }
    }
    if out.is_empty() {
        "sala".to_owned()
    } else {
        out
    }
}

pub(super) fn unique_room_id(title: &str, existing: &[String]) -> String {
    let base = slugify_room_title(title);
    if !existing.iter().any(|id| id == &base) {
        return base;
    }
    for n in 2..=(MAX_ROOMS_COUNT + 2) {
        let suffix = format!("-{n}");
        let max_prefix = MAX_ROOM_NAME_LEN.saturating_sub(suffix.len());
        let mut prefix: String = base.chars().take(max_prefix).collect();
        while prefix.ends_with('-') {
            prefix.pop();
        }
        let candidate = if prefix.is_empty() {
            format!("sala{suffix}")
        } else {
            format!("{prefix}{suffix}")
        };
        if !existing.iter().any(|id| id == &candidate) {
            return candidate;
        }
    }
    base
}
