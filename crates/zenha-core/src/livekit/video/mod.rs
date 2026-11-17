//! Compartilhamento de tela e recepção de vídeo sobre o SDK do LiveKit.

mod capture;
mod convert;
mod publish;
mod watch;

pub(crate) use capture::DesktopCaptureSource;
pub(crate) use publish::{LocalShare, publish_share, stop_share};
pub(crate) use watch::Watch;
