//! 检查器抽象。一个 `Check` 对应一类关注点，产出若干条 `Finding`。

use crate::config::Config;
use crate::model::Finding;

pub struct Ctx<'a> {
    pub cfg: &'a Config,
}

pub trait Check {
    /// 稳定标识，用于 `--only` 过滤和日志。
    fn id(&self) -> &'static str;
    /// 人读标题。
    fn title(&self) -> &'static str;
    /// 执行检查。实现里不应 panic：任何读取失败都要降级成 `Skip`。
    fn run(&self, cx: &Ctx<'_>) -> Vec<Finding>;
}

pub mod registry {
    use super::Check;
    use crate::checks;

    /// 检查器注册表。新增一类检查只需要在这里加一行。
    pub fn all() -> Vec<Box<dyn Check>> {
        vec![
            Box::new(checks::grub::GrubCheck),
            Box::new(checks::kernel::KernelCheck),
            Box::new(checks::virt::VirtCheck),
            Box::new(checks::space::SpaceCheck),
            Box::new(checks::device::DeviceCheck),
            Box::new(checks::network::NetworkCheck),
            Box::new(checks::root::RootCheck),
            Box::new(checks::process::ProcessCheck),
        ]
    }
}
