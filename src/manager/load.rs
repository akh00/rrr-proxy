use std::error::Error;

use sysinfo::{CpuRefreshKind, RefreshKind, System};
pub struct ReportLoad {
    percent: f32,
}

pub trait ReportLoadT {
    fn get_load(&mut self) -> Result<ReportLoad, Box<dyn Error + Send + Sync>>;
}

pub struct ReportLoadSysProvider {
    system: System,
}

impl ReportLoadSysProvider {
    pub fn new() -> ReportLoadSysProvider {
        let system = System::new_with_specifics(
            RefreshKind::nothing().with_cpu(CpuRefreshKind::everything()),
        );
        ReportLoadSysProvider { system }
    }
}
impl ReportLoadT for ReportLoadSysProvider {
    fn get_load(&mut self) -> Result<ReportLoad, Box<dyn Error + Send + Sync>> {
        self.system.refresh_cpu_usage();
        Ok(ReportLoad {
            percent: self.system.global_cpu_usage(),
        })
    }
}
