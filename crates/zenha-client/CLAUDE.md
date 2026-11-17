# zenha-client

Interface desktop em `eframe`/`egui`.

- Nunca importa `livekit`. Depende só de `zenha-core` e `zenha-protocol`.
- `fn ui` não bloqueia: sem rede, sem disco, sem `lock` demorado. Lê os `Event` pendentes com `try_recv` a cada frame.
- O estado de tela é um `enum`; transições acontecem em um lugar só.
- Texto da interface em português.
- `eframe` 0.37 usa `fn ui(&mut self, ui: &mut egui::Ui, ...)` e os painéis recebem `ui`, não `ctx`.
