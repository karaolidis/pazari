use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EngineError {
    #[error(
        "{plugged} bytes plugged cannot cover {service_reserve} bytes of service reserve plus {burst_reserve} bytes of burst reserve and a {block_size} byte block for the workload"
    )]
    ReservesExceedMemory {
        plugged: u64,
        service_reserve: u64,
        burst_reserve: u64,
        block_size: u64,
    },

    #[error("{plugged} bytes plugged is below the {boot_mem} bytes the guest booted with")]
    PluggedBelowBoot { plugged: u64, boot_mem: u64 },

    #[error(
        "a {open_guard} byte open guard leaves nothing of the {burst_reserve} byte burst reserve to release"
    )]
    GuardSwallowsReserve { open_guard: u64, burst_reserve: u64 },
}
