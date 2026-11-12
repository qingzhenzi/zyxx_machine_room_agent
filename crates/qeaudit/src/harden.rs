//! 进程自我保护（Linux 原生机制，零依赖）。
//!
//! 这几项都是「启动时设置一次、之后无法回退」的：
//! - `PR_SET_DUMPABLE(0)`：禁止他人 ptrace / 读取 /proc/pid/mem，防止被注入；
//! - `PR_SET_NO_NEW_PRIVS(1)`：setuid 程序不再提权，也是 seccomp 的前置条件；
//! - `mlockall`：锁定地址空间，避免密钥/策略被换出到 swap 落盘；
//! - `RLIMIT_CORE=0`：崩溃不产生 core，避免内存里的凭据落到磁盘。

pub struct Step {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

/// 按顺序施加加固。返回每一步的结果，**不 panic、不中止**：
/// 某些项在受限容器里必然失败，属于可接受的降级。
pub fn apply() -> Vec<Step> {
    let mut steps = Vec::new();

    // SAFETY: prctl 是纯内核接口，参数按手册取值，无内存安全风险。
    unsafe {
        let r = libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
        steps.push(Step {
            name: "PR_SET_DUMPABLE=0",
            ok: r == 0,
            detail: if r == 0 {
                "禁止 ptrace/读取进程内存".into()
            } else {
                format!("失败: {}", std::io::Error::last_os_error())
            },
        });

        let r = libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
        steps.push(Step {
            name: "PR_SET_NO_NEW_PRIVS=1",
            ok: r == 0,
            detail: if r == 0 {
                "禁止通过 setuid 提权".into()
            } else {
                format!("失败: {}", std::io::Error::last_os_error())
            },
        });

        // MCL_CURRENT | MCL_FUTURE：当前与未来的映射都锁定
        let r = libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE);
        steps.push(Step {
            name: "mlockall",
            ok: r == 0,
            detail: if r == 0 {
                "地址空间已锁定，不进入 swap".into()
            } else {
                format!(
                    "失败（需 CAP_IPC_LOCK）：{}",
                    std::io::Error::last_os_error()
                )
            },
        });

        let lim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        let r = libc::setrlimit(libc::RLIMIT_CORE, &lim);
        steps.push(Step {
            name: "RLIMIT_CORE=0",
            ok: r == 0,
            detail: if r == 0 {
                "崩溃不生成 core dump".into()
            } else {
                format!("失败: {}", std::io::Error::last_os_error())
            },
        });
    }

    steps
}

/// 打印加固结果，供 `--harden` 使用。
pub fn print_steps(steps: &[Step]) {
    for s in steps {
        let mark = if s.ok { "ok  " } else { "warn" };
        eprintln!("[{mark}] {:<28} {}", s.name, s.detail);
    }
}
