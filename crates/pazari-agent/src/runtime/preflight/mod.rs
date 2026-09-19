use std::{fmt, fs};

use pazari_types::Swappiness;

use crate::runtime::{
    cgroup::{
        Limit, MEMORY_CURRENT, MEMORY_EVENTS, MEMORY_HIGH, MEMORY_MAX, MEMORY_OOM_GROUP,
        MEMORY_PRESSURE, MEMORY_RECLAIM, MEMORY_SWAP_CURRENT, MEMORY_SWAP_MAX, parse,
    },
    config::RuntimeConfig,
};

const CONTROLLERS: &str = "cgroup.controllers";
const SUBTREE_CONTROL: &str = "cgroup.subtree_control";

/// The prefix of the `/proc/self/cgroup` line that names a cgroup v2 path.
const UNIFIED_ENTRY: &str = "0::";

const READS: [&str; 3] = [MEMORY_EVENTS, MEMORY_CURRENT, MEMORY_PRESSURE];

const WRITES: [&str; 4] = [MEMORY_HIGH, MEMORY_MAX, MEMORY_OOM_GROUP, MEMORY_RECLAIM];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Pass,

    Warn,

    Fail,
}

impl fmt::Display for Severity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Pass => "pass",
            Self::Warn => "warn",
            Self::Fail => "fail",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: &'static str,
    pub severity: Severity,
    pub detail: String,
}

impl fmt::Display for Check {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} {}: {}",
            self.severity, self.name, self.detail
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    checks: Vec<Check>,
}

impl Report {
    #[must_use]
    pub fn checks(&self) -> &[Check] {
        &self.checks
    }

    #[must_use]
    pub fn severity(&self) -> Severity {
        self.checks
            .iter()
            .map(|check| check.severity)
            .max()
            .unwrap_or(Severity::Pass)
    }

    #[must_use]
    pub fn passed(&self) -> bool {
        self.severity() != Severity::Fail
    }
}

impl fmt::Display for Report {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, check) in self.checks.iter().enumerate() {
            if index > 0 {
                writeln!(formatter)?;
            }

            write!(formatter, "{check}")?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Preflight<'a> {
    config: &'a RuntimeConfig,
}

impl<'a> Preflight<'a> {
    #[must_use]
    pub const fn new(config: &'a RuntimeConfig) -> Self {
        Self { config }
    }

    #[must_use]
    pub fn run(self, swappiness: Option<Swappiness>) -> Report {
        Report {
            checks: vec![
                self.cgroup_v2(),
                self.workload_cgroup(),
                self.agent_cgroup(),
                self.memory_controller(),
                self.memmap_on_memory(),
                self.transparent_hugepages(),
                self.block_size(),
                self.swap(swappiness),
            ],
        }
    }

    fn cgroup_v2(self) -> Check {
        const NAME: &str = "cgroup v2";

        let root = &self.config.paths.cgroup_root;
        let controllers = root.join(CONTROLLERS);

        if controllers.is_file() {
            return pass(NAME, format!("{} is a unified hierarchy", root.display()));
        }

        fail(
            NAME,
            format!(
                "{} carries no {CONTROLLERS}: cgroup v2 is not mounted there",
                root.display()
            ),
        )
    }

    fn workload_cgroup(self) -> Check {
        const NAME: &str = "workload cgroup";

        let cgroup = &self.config.cgroup;

        if !cgroup.is_dir() {
            return fail(
                NAME,
                format!("{} is not a cgroup directory", cgroup.display()),
            );
        }

        let mut faults = Vec::new();

        for name in READS {
            if let Err(error) = fs::File::open(cgroup.join(name)) {
                faults.push(format!("{name} is not readable: {error}"));
            }
        }

        for name in WRITES {
            if let Err(error) = fs::OpenOptions::new().write(true).open(cgroup.join(name)) {
                faults.push(format!("{name} is not writable: {error}"));
            }
        }

        if faults.is_empty() {
            return pass(
                NAME,
                format!(
                    "{} has every memory.* file the agent uses",
                    cgroup.display()
                ),
            );
        }

        fail(NAME, format!("{}: {}", cgroup.display(), faults.join("; ")))
    }

    fn agent_cgroup(self) -> Check {
        const NAME: &str = "agent cgroup";

        let path = &self.config.paths.self_cgroup;
        let workload = &self.config.cgroup;

        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                return warn(NAME, format!("cannot read {}: {error}", path.display()));
            }
        };

        let Some(own) = text
            .lines()
            .find_map(|line| line.strip_prefix(UNIFIED_ENTRY))
            .map(str::trim)
        else {
            return warn(
                NAME,
                format!(
                    "{} carries no {UNIFIED_ENTRY} line: the agent cannot tell which cgroup it is in",
                    path.display()
                ),
            );
        };

        if own == "/" {
            return warn(
                NAME,
                format!(
                    "{} reports the hierarchy root, which is what a cgroup namespace reports too: the agent cannot tell it is outside {}",
                    path.display(),
                    workload.display()
                ),
            );
        }

        let own = self
            .config
            .paths
            .cgroup_root
            .join(own.trim_start_matches('/'));

        if !own.is_dir() {
            return warn(
                NAME,
                format!(
                    "{} is not a cgroup directory: the agent is in a cgroup namespace and cannot tell it is outside {}",
                    own.display(),
                    workload.display()
                ),
            );
        }

        if own.starts_with(workload) {
            return fail(
                NAME,
                format!(
                    "the agent runs in {}, inside the cgroup it brakes: memory.high would throttle the process that has to lift it",
                    own.display()
                ),
            );
        }

        pass(
            NAME,
            format!(
                "the agent runs in {}, outside {}",
                own.display(),
                workload.display()
            ),
        )
    }

    fn memory_controller(self) -> Check {
        const NAME: &str = "memory controller";

        let Some(parent) = self.config.cgroup.parent() else {
            return fail(
                NAME,
                format!(
                    "{} is a hierarchy root and has no parent to delegate the controller",
                    self.config.cgroup.display()
                ),
            );
        };

        let control = parent.join(SUBTREE_CONTROL);

        let enabled = match fs::read_to_string(&control) {
            Ok(text) => text,
            Err(error) => {
                return fail(NAME, format!("cannot read {}: {error}", control.display()));
            }
        };

        if enabled.split_whitespace().any(|name| name == "memory") {
            return pass(
                NAME,
                format!("{} delegates memory to its children", parent.display()),
            );
        }

        fail(
            NAME,
            format!(
                "{} does not list memory in {SUBTREE_CONTROL}: the workload cgroup gets no memory.* files",
                parent.display()
            ),
        )
    }

    fn memmap_on_memory(self) -> Check {
        const NAME: &str = "memmap_on_memory";

        let path = &self.config.paths.memmap_on_memory;

        let value = match fs::read_to_string(path) {
            Ok(text) => text.trim().to_owned(),
            Err(error) => {
                return fail(NAME, format!("cannot read {}: {error}", path.display()));
            }
        };

        match value.as_str() {
            "Y" | "1" => pass(NAME, "a hotplugged range carries its own memmap".to_owned()),
            "N" | "0" => fail(
                NAME,
                format!(
                    "{} is {value}: a hotplugged range takes its memmap out of the memory the guest already has",
                    path.display()
                ),
            ),
            _ => fail(
                NAME,
                format!("{} holds {value:?}, which is not Y or N", path.display()),
            ),
        }
    }

    fn transparent_hugepages(self) -> Check {
        const NAME: &str = "transparent hugepages";

        let path = &self.config.paths.transparent_hugepage;

        let value = match fs::read_to_string(path) {
            Ok(text) => text.trim().to_owned(),
            Err(error) => {
                return fail(NAME, format!("cannot read {}: {error}", path.display()));
            }
        };

        let mode = value
            .split_whitespace()
            .find_map(|token| token.strip_prefix('[')?.strip_suffix(']'));

        let Some(mode) = mode else {
            return fail(
                NAME,
                format!("{} holds {value:?}, which selects no mode", path.display()),
            );
        };

        if mode != "always" {
            return pass(NAME, format!("the mode is {mode}"));
        }

        let detail = format!(
            "{} is set to always: the workload faults in 2 MiB at a time and overshoots memory.high",
            path.display()
        );

        if self.config.thp_always_fatal {
            fail(NAME, detail)
        } else {
            warn(NAME, detail)
        }
    }

    fn block_size(self) -> Check {
        const NAME: &str = "block size";

        let path = &self.config.paths.block_size;

        let value = match fs::read_to_string(path) {
            Ok(text) => text.trim().to_owned(),
            Err(error) => {
                return fail(NAME, format!("cannot read {}: {error}", path.display()));
            }
        };

        match u64::from_str_radix(&value, 16) {
            Ok(bytes) if bytes > 0 => pass(NAME, format!("blocks are {bytes} bytes")),
            _ => fail(
                NAME,
                format!(
                    "{} holds {value:?}, which is not a positive hexadecimal byte count",
                    path.display()
                ),
            ),
        }
    }

    fn swap(self, swappiness: Option<Swappiness>) -> Check {
        const NAME: &str = "swap";

        if swappiness == Some(Swappiness::NONE) {
            return pass(
                NAME,
                "reclaim runs at swappiness=0 and never swaps".to_owned(),
            );
        }

        let intent = swappiness.map_or_else(
            || "reclaim runs at the guest's own vm.swappiness".to_owned(),
            |swappiness| format!("reclaim runs at swappiness={swappiness}"),
        );

        let meminfo = &self.config.paths.meminfo;

        let total = match fs::read_to_string(meminfo) {
            Ok(text) => text.lines().find_map(|line| {
                let rest = line.strip_prefix("SwapTotal:")?;
                rest.split_whitespace().next()?.parse().ok()
            }),
            Err(error) => {
                return warn(NAME, format!("cannot read {}: {error}", meminfo.display()));
            }
        };

        if total == Some(0) || total.is_none() {
            return warn(
                NAME,
                format!("{intent}, and {} reports no swap", meminfo.display()),
            );
        }

        let path = self.config.cgroup.join(MEMORY_SWAP_MAX);

        if let Ok(text) = fs::read_to_string(&path)
            && parse::limit(&path, &text).ok().and_then(Limit::bytes) == Some(0)
        {
            return warn(NAME, format!("{intent}, and {} is 0", path.display()));
        }

        if !self.config.cgroup.join(MEMORY_SWAP_CURRENT).is_file() {
            return warn(
                NAME,
                format!(
                    "{intent}, and {} has no {MEMORY_SWAP_CURRENT}: swap accounting is off and the agent cannot see what it freed",
                    self.config.cgroup.display()
                ),
            );
        }

        pass(NAME, format!("{intent}, and the guest has swap to use"))
    }
}

const fn pass(name: &'static str, detail: String) -> Check {
    Check {
        name,
        severity: Severity::Pass,
        detail,
    }
}

const fn warn(name: &'static str, detail: String) -> Check {
    Check {
        name,
        severity: Severity::Warn,
        detail,
    }
}

const fn fail(name: &'static str, detail: String) -> Check {
    Check {
        name,
        severity: Severity::Fail,
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::testing::Guest;

    fn severity(report: &Report, name: &str) -> Severity {
        report
            .checks()
            .iter()
            .find(|check| check.name == name)
            .map(|check| check.severity)
            .expect("the check ran")
    }

    fn detail(report: &Report, name: &str) -> String {
        report
            .checks()
            .iter()
            .find(|check| check.name == name)
            .map(|check| check.detail.clone())
            .expect("the check ran")
    }

    #[test]
    fn a_guest_that_is_set_up_passes_every_check() {
        let guest = Guest::new();
        let report = Preflight::new(&guest.config).run(Some(Swappiness::NONE));

        assert_eq!(report.severity(), Severity::Pass, "{report}");
        assert!(report.passed());
        assert_eq!(report.checks().len(), 8);
    }

    #[test]
    fn a_hierarchy_that_is_not_cgroup_v2_fails() {
        let guest = Guest::new();
        Guest::remove(&guest.config.paths.cgroup_root.join(CONTROLLERS));

        let report = Preflight::new(&guest.config).run(None);

        assert_eq!(severity(&report, "cgroup v2"), Severity::Fail);
        assert!(detail(&report, "cgroup v2").contains("cgroup v2 is not mounted"));
        assert!(!report.passed());
    }

    #[test]
    fn a_workload_cgroup_missing_a_file_fails_and_names_it() {
        let guest = Guest::new();
        Guest::remove(&guest.config.cgroup.join(MEMORY_RECLAIM));

        let report = Preflight::new(&guest.config).run(None);

        assert_eq!(severity(&report, "workload cgroup"), Severity::Fail);
        assert!(
            detail(&report, "workload cgroup").contains("memory.reclaim is not writable"),
            "{}",
            detail(&report, "workload cgroup")
        );
    }

    #[test]
    fn a_workload_cgroup_that_is_not_there_fails() {
        let guest = Guest::new();
        let config = RuntimeConfig {
            cgroup: guest.config.cgroup.join("absent"),
            ..guest.config.clone()
        };

        let report = Preflight::new(&config).run(None);

        assert_eq!(severity(&report, "workload cgroup"), Severity::Fail);
    }

    #[test]
    fn an_agent_in_the_cgroup_it_brakes_fails() {
        let guest = Guest::new();
        Guest::write(&guest.config.paths.self_cgroup, "0::/workload.slice\n");

        let report = Preflight::new(&guest.config).run(None);

        assert_eq!(severity(&report, "agent cgroup"), Severity::Fail);
        assert!(detail(&report, "agent cgroup").contains("inside the cgroup it brakes"));
        assert!(!report.passed());
    }

    #[test]
    fn an_agent_under_the_cgroup_it_brakes_fails() {
        let guest = Guest::new();
        Guest::dir(&guest.config.cgroup.join("agent.service"));
        Guest::write(
            &guest.config.paths.self_cgroup,
            "0::/workload.slice/agent.service\n",
        );

        assert_eq!(
            severity(&Preflight::new(&guest.config).run(None), "agent cgroup"),
            Severity::Fail
        );
    }

    #[test]
    fn an_agent_in_a_cgroup_that_only_shares_a_prefix_passes() {
        let guest = Guest::new();
        Guest::dir(&guest.config.paths.cgroup_root.join("workload.slice2"));
        Guest::write(&guest.config.paths.self_cgroup, "0::/workload.slice2\n");

        assert_eq!(
            severity(&Preflight::new(&guest.config).run(None), "agent cgroup"),
            Severity::Pass
        );
    }

    #[test]
    fn an_agent_that_cannot_place_itself_warns_rather_than_guess() {
        let guest = Guest::new();

        for text in ["0::/\n", "0::/elsewhere.slice\n", "3:memory:/workload\n"] {
            Guest::write(&guest.config.paths.self_cgroup, text);

            let report = Preflight::new(&guest.config).run(None);

            assert_eq!(
                severity(&report, "agent cgroup"),
                Severity::Warn,
                "did not warn on {text:?}"
            );
            assert!(report.passed(), "a warning still starts the agent");
        }

        Guest::remove(&guest.config.paths.self_cgroup);

        assert_eq!(
            severity(&Preflight::new(&guest.config).run(None), "agent cgroup"),
            Severity::Warn
        );
    }

    #[test]
    fn a_parent_that_withholds_the_memory_controller_fails() {
        let guest = Guest::new();
        Guest::write(
            &guest
                .config
                .cgroup
                .parent()
                .expect("the fixture nests")
                .join(SUBTREE_CONTROL),
            "cpu pids\n",
        );

        let report = Preflight::new(&guest.config).run(None);

        assert_eq!(severity(&report, "memory controller"), Severity::Fail);
        assert!(detail(&report, "memory controller").contains("does not list memory"));
    }

    #[test]
    fn a_kernel_without_memmap_on_memory_fails() {
        let guest = Guest::new();
        Guest::write(&guest.config.paths.memmap_on_memory, "N\n");

        let report = Preflight::new(&guest.config).run(None);

        assert_eq!(severity(&report, "memmap_on_memory"), Severity::Fail);
        assert!(detail(&report, "memmap_on_memory").contains("takes its memmap out of"));

        Guest::write(&guest.config.paths.memmap_on_memory, "1\n");
        assert_eq!(
            severity(&Preflight::new(&guest.config).run(None), "memmap_on_memory"),
            Severity::Pass
        );

        Guest::remove(&guest.config.paths.memmap_on_memory);
        assert_eq!(
            severity(&Preflight::new(&guest.config).run(None), "memmap_on_memory"),
            Severity::Fail
        );
    }

    #[test]
    fn hugepages_set_to_always_fail_unless_the_deployment_says_otherwise() {
        let guest = Guest::new();
        Guest::write(
            &guest.config.paths.transparent_hugepage,
            "[always] madvise never\n",
        );

        let report = Preflight::new(&guest.config).run(None);

        assert_eq!(severity(&report, "transparent hugepages"), Severity::Fail);
        assert!(detail(&report, "transparent hugepages").contains("overshoots memory.high"));

        let lenient = RuntimeConfig {
            thp_always_fatal: false,
            ..guest.config
        };

        assert_eq!(
            severity(&Preflight::new(&lenient).run(None), "transparent hugepages"),
            Severity::Warn
        );
    }

    #[test]
    fn hugepages_on_madvise_or_never_pass() {
        let guest = Guest::new();

        for mode in ["always madvise [never]", "always [madvise] never"] {
            Guest::write(&guest.config.paths.transparent_hugepage, mode);

            assert_eq!(
                severity(
                    &Preflight::new(&guest.config).run(None),
                    "transparent hugepages"
                ),
                Severity::Pass,
                "rejected {mode}"
            );
        }

        Guest::write(&guest.config.paths.transparent_hugepage, "always madvise\n");

        assert_eq!(
            severity(
                &Preflight::new(&guest.config).run(None),
                "transparent hugepages"
            ),
            Severity::Fail
        );
    }

    #[test]
    fn a_block_size_that_does_not_parse_fails() {
        let guest = Guest::new();

        assert_eq!(
            severity(&Preflight::new(&guest.config).run(None), "block size"),
            Severity::Pass
        );
        assert_eq!(
            detail(&Preflight::new(&guest.config).run(None), "block size"),
            "blocks are 134217728 bytes"
        );

        Guest::write(&guest.config.paths.block_size, "0x8000000\n");

        assert_eq!(
            severity(&Preflight::new(&guest.config).run(None), "block size"),
            Severity::Fail
        );
    }

    #[test]
    fn swapping_reclaim_on_a_guest_without_swap_warns() {
        let guest = Guest::new();
        Guest::write(
            &guest.config.paths.meminfo,
            "MemTotal:       2096756 kB\nMemAvailable:   1729396 kB\nSwapTotal:            0 kB\n",
        );

        let report = Preflight::new(&guest.config).run(None);

        assert_eq!(severity(&report, "swap"), Severity::Warn);
        assert!(detail(&report, "swap").contains("reports no swap"));
        assert!(report.passed(), "a warning still starts the agent");
    }

    #[test]
    fn swapping_reclaim_against_a_swap_max_of_zero_warns() {
        let guest = Guest::new();
        Guest::write(&guest.config.cgroup.join(MEMORY_SWAP_MAX), "0\n");

        let swappiness = Swappiness::try_new(60).expect("60 is a swappiness");
        let report = Preflight::new(&guest.config).run(Some(swappiness));

        assert_eq!(severity(&report, "swap"), Severity::Warn);
        assert!(detail(&report, "swap").contains("swappiness=60"));
    }

    #[test]
    fn swapping_reclaim_with_swap_to_use_passes() {
        let guest = Guest::new();
        let swappiness = Swappiness::try_new(60).expect("60 is a swappiness");

        assert_eq!(
            severity(&Preflight::new(&guest.config).run(Some(swappiness)), "swap"),
            Severity::Pass
        );
    }

    #[test]
    fn a_report_prints_one_line_a_check() {
        let report = Report {
            checks: vec![
                pass("cgroup v2", "a unified hierarchy".to_owned()),
                warn("swap", "no swap".to_owned()),
            ],
        };

        assert_eq!(
            report.to_string(),
            "pass cgroup v2: a unified hierarchy\nwarn swap: no swap"
        );
        assert_eq!(report.severity(), Severity::Warn);
    }

    #[test]
    fn an_empty_report_passes() {
        let report = Report { checks: Vec::new() };

        assert_eq!(report.severity(), Severity::Pass);
        assert_eq!(report.to_string(), "");
    }
}
