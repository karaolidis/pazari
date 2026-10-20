{
  description = "a guest on transparent_hugepage=always is refused by preflight, and faults in 2MiB when it is not";

  timeout = 300;

  daemon.settings.grant = {
    floor = "2GiB";
    min_interval = "2s";
  };

  guests.main = {
    daemon.ceiling = "3GiB";

    module = { config, lib, ... }: {
      boot.kernelParams = lib.mkAfter [ "transparent_hugepage=always" ];

      environment.systemPackages = [ config.services.pazari-agent.package ];

      services.pazari-agent = {
        settings.preflight.thp_always_fatal = false;

        harness.workload.phases = [
          "grow=1GiB,rate=256MiB/s"
          "hold=30s"
        ];
      };
    };
  };

  assertions = ''
    from pazari_harness.checks import BLOCK, grew

    SELECTED = "/sys/kernel/mm/transparent_hugepage/enabled"
    STRICT = "/tmp/pazari-strict.toml"

    def check(run):
        guest = run.guest

        _, selected = guest.shell(f"cat {SELECTED}", "the guest's hugepage mode was read")

        run.require(
            "[always]" in selected,
            f"the guest really is on transparent_hugepage=always: {selected.strip()!r}",
        )

        guest.shell(
            "sed 's/^thp_always_fatal = false/thp_always_fatal = true/'"
            f" /etc/pazari/agent.toml > {STRICT}",
            "the shipped posture was put back into a copy of the agent's config",
        )

        code, report = guest.shell(
            f"pazari-agent preflight --config {STRICT}",
            "preflight was run under the shipped posture",
            expect=None,
        )

        run.require(
            code != 0,
            "preflight refuses this guest, so the agent would never have started on it",
        )
        run.require(
            "fail transparent hugepages" in report,
            f"preflight names transparent hugepages as the reason: {report.strip()[-200:]!r}",
        )

        size = run.daemon.gauge("pazarid_guest_size_bytes", guest.labels)

        grew(run, guest, size)

        _, anon = guest.shell(
            "grep -E '^(anon|anon_thp) ' /sys/fs/cgroup/workload.slice/memory.stat",
            "the workload's cgroup was asked how it faulted its memory in",
        )

        stat = dict(
            (line.split()[0], int(line.split()[1])) for line in anon.split("\n") if line
        )

        run.require(
            stat["anon_thp"] >= stat["anon"] // 2,
            "most of what the workload touched came in as 2MiB pages, which is what "
            f"overshoots memory.high: anon_thp={stat['anon_thp']} of anon={stat['anon']}",
        )
        run.require(
            stat["anon_thp"] >= BLOCK,
            f"those huge pages account for {stat['anon_thp']} bytes, more than a whole "
            "hotplug block the brake can never see coming",
        )
  '';
}
