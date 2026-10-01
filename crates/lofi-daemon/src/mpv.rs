pub trait MpvController {
    fn start_source(&mut self, source: &str) -> anyhow::Result<()>;
    fn stop(&mut self) -> anyhow::Result<()>;
    fn pause(&mut self) -> anyhow::Result<()>;
    fn resume(&mut self) -> anyhow::Result<()>;
    fn last_error(&self) -> Option<String>;
}
