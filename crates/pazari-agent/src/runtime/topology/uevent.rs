use std::{io, os::fd::OwnedFd, str};

use pazari_types::{Clock, Interval};
use rustix::net::{
    AddressFamily, RecvFlags, SocketFlags, SocketType, bind,
    netlink::{KOBJECT_UEVENT, SocketAddrNetlink},
    recv, socket_with,
};
use tokio::{
    io::unix::AsyncFd,
    time::{Instant, timeout_at},
};

use crate::{
    engine::Event,
    runtime::{
        error::{NetlinkStep, RuntimeError},
        topology::Topology,
    },
};

/// The kernel's own uevent multicast group.
const KERNEL_GROUP: u32 = 1;

/// `UEVENT_BUFFER_SIZE`: the kernel will not multicast a longer payload.
const PAYLOAD: usize = 2048;

/// Whether a uevent adds, removes, onlines or offlines a memory block.
fn is_memory_hotplug(payload: &[u8]) -> bool {
    let mut subsystem = None;
    let mut action = None;

    for field in payload.split(|byte| *byte == 0).skip(1) {
        let Ok(field) = str::from_utf8(field) else {
            continue;
        };

        let Some((key, value)) = field.split_once('=') else {
            continue;
        };

        match key {
            "SUBSYSTEM" => subsystem = Some(value),
            "ACTION" => action = Some(value),
            _ => {}
        }
    }

    subsystem == Some("memory") && matches!(action, Some("add" | "remove" | "online" | "offline"))
}

/// Waits out a burst of memory hotplugs, keeping its place if it is dropped
/// part way through.
#[derive(Debug)]
struct Settle {
    debounce: Interval,

    /// When the hotplugs heard so far count as settled.
    quiet: Option<Instant>,
}

impl Settle {
    const fn new(debounce: Interval) -> Self {
        Self {
            debounce,
            quiet: None,
        }
    }

    /// Waits for a hotplug from `next`, then until `debounce` passes without
    /// another.
    async fn settled(
        &mut self,
        mut next: impl AsyncFnMut() -> Result<(), RuntimeError>,
    ) -> Result<(), RuntimeError> {
        loop {
            let heard = match self.quiet {
                None => next().await,
                Some(quiet) => match timeout_at(quiet, next()).await {
                    Ok(heard) => heard,
                    Err(_) => return Ok(()),
                },
            };

            heard?;

            self.quiet = Some(Instant::now() + self.debounce.get());
        }
    }

    const fn reset(&mut self) {
        self.quiet = None;
    }
}

/// Watches the kobject uevent socket for the guest's memory changing size.
#[derive(Debug)]
pub struct HotplugListener {
    uevents: Uevents,
    topology: Topology,
    settle: Settle,
    clock: Clock,
}

impl HotplugListener {
    pub fn bind(
        topology: Topology,
        debounce: Interval,
        clock: Clock,
    ) -> Result<Self, RuntimeError> {
        let socket = socket_with(
            AddressFamily::NETLINK,
            SocketType::DGRAM,
            SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
            Some(KOBJECT_UEVENT),
        )
        .map_err(|source| RuntimeError::Netlink {
            step: NetlinkStep::Open,
            source: source.into(),
        })?;

        bind(&socket, &SocketAddrNetlink::new(0, KERNEL_GROUP)).map_err(|source| {
            RuntimeError::Netlink {
                step: NetlinkStep::Bind,
                source: source.into(),
            }
        })?;

        let socket = AsyncFd::new(socket).map_err(|source| RuntimeError::Netlink {
            step: NetlinkStep::Watch,
            source,
        })?;

        Ok(Self {
            uevents: Uevents(socket),
            topology,
            settle: Settle::new(debounce),
            clock,
        })
    }

    pub async fn changed(&mut self) -> Result<Event, RuntimeError> {
        let mut payload = [0_u8; PAYLOAD];

        self.settle
            .settled(async || self.uevents.hotplug(&mut payload).await)
            .await?;

        let total_mem = self.topology.total_mem().await?;

        self.settle.reset();

        Ok(Event::TotalChanged {
            now: self.clock.now(),
            total_mem,
        })
    }
}

/// The kobject uevent socket.
#[derive(Debug)]
struct Uevents(AsyncFd<OwnedFd>);

impl Uevents {
    /// Reads uevents until one is a memory hotplug.
    async fn hotplug(&self, payload: &mut [u8]) -> Result<(), RuntimeError> {
        loop {
            let len = self.recv(payload).await?;

            if is_memory_hotplug(&payload[..len]) {
                return Ok(());
            }
        }
    }

    async fn recv(&self, payload: &mut [u8]) -> Result<usize, RuntimeError> {
        loop {
            let mut ready = self
                .0
                .readable()
                .await
                .map_err(|source| RuntimeError::Netlink {
                    step: NetlinkStep::Wait,
                    source,
                })?;

            let read = ready.try_io(|socket| {
                recv(socket.get_ref(), &mut *payload, RecvFlags::empty())
                    .map(|(len, _)| len)
                    .map_err(io::Error::from)
            });

            if let Ok(result) = read {
                return result.map_err(|source| RuntimeError::Netlink {
                    step: NetlinkStep::Read,
                    source,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{future, time::Duration};

    use tokio::time::sleep;

    use super::*;
    use crate::engine::testing::interval_ms;

    fn payload(header: &str, fields: &[&str]) -> Vec<u8> {
        let mut bytes = header.as_bytes().to_vec();

        for field in fields {
            bytes.push(0);
            bytes.extend_from_slice(field.as_bytes());
        }

        bytes.push(0);
        bytes
    }

    fn memory(action: &str) -> Vec<u8> {
        payload(
            &format!("{action}@/devices/system/memory/memory42"),
            &[
                &format!("ACTION={action}"),
                "DEVPATH=/devices/system/memory/memory42",
                "SUBSYSTEM=memory",
                "SEQNUM=4831",
            ],
        )
    }

    #[test]
    fn every_memory_hotplug_is_recognised() {
        for action in ["add", "remove", "online", "offline"] {
            assert!(is_memory_hotplug(&memory(action)), "{action}");
        }
    }

    #[test]
    fn another_subsystem_is_not_a_hotplug() {
        let block = payload(
            "add@/devices/pci0000:00/virtio2/block/vda",
            &[
                "ACTION=add",
                "DEVPATH=/devices/pci0000:00/virtio2/block/vda",
                "SUBSYSTEM=block",
                "DEVNAME=vda",
            ],
        );

        assert!(!is_memory_hotplug(&block));
    }

    #[test]
    fn a_memory_event_that_moves_nothing_is_not_a_hotplug() {
        let change = payload(
            "change@/devices/system/memory/memory42",
            &[
                "ACTION=change",
                "SUBSYSTEM=memory",
                "DEVPATH=/devices/system/memory/memory42",
            ],
        );

        assert!(!is_memory_hotplug(&change));
    }

    #[test]
    fn a_payload_without_the_properties_that_name_it_is_not_a_hotplug() {
        assert!(!is_memory_hotplug(b""));
        assert!(!is_memory_hotplug(b"add@/devices/system/memory/memory42\0"));
        assert!(!is_memory_hotplug(&payload(
            "add@/devices/system/memory/memory42",
            &["ACTION=add", "DEVPATH=/devices/system/memory/memory42"],
        )));
        assert!(!is_memory_hotplug(&payload(
            "add@/devices/system/memory/memory42",
            &["SUBSYSTEM=memory"],
        )));
    }

    const DEBOUNCE: Duration = Duration::from_millis(100);

    fn settle() -> Settle {
        Settle::new(interval_ms(100))
    }

    #[tokio::test(start_paused = true)]
    async fn a_settle_ends_one_debounce_after_the_last_hotplug() {
        let mut settle = settle();
        let mut heard = 0;
        let start = Instant::now();

        settle
            .settled(async || {
                heard += 1;

                if heard > 1 {
                    future::pending::<()>().await;
                }

                Ok(())
            })
            .await
            .expect("nothing fails");

        assert_eq!(start.elapsed(), DEBOUNCE);
    }

    #[tokio::test(start_paused = true)]
    async fn each_memory_hotplug_starts_the_settle_over() {
        let mut settle = settle();
        let mut heard = 0;
        let start = Instant::now();

        settle
            .settled(async || {
                heard += 1;

                if heard > 4 {
                    future::pending::<()>().await;
                }

                if heard > 1 {
                    sleep(Duration::from_millis(50)).await;
                }

                Ok(())
            })
            .await
            .expect("nothing fails");

        assert_eq!(heard, 5, "the settle ended before the hotplugs did");
        assert_eq!(start.elapsed(), Duration::from_millis(250));
    }

    #[tokio::test(start_paused = true)]
    async fn a_settle_dropped_part_way_keeps_the_hotplug_it_heard() {
        let mut settle = settle();
        let mut heard = 0;
        let start = Instant::now();

        let dropped = timeout_at(
            start + Duration::from_millis(30),
            settle.settled(async || {
                heard += 1;

                if heard > 1 {
                    future::pending::<()>().await;
                }

                Ok(())
            }),
        )
        .await;

        assert!(dropped.is_err(), "the settle ended inside its debounce");

        settle
            .settled(async || future::pending().await)
            .await
            .expect("nothing fails");

        assert_eq!(start.elapsed(), DEBOUNCE);
    }

    #[test]
    fn a_property_that_is_not_utf8_is_stepped_over() {
        let mut bytes = b"add@/devices/system/memory/memory42\0DEVNAME=".to_vec();
        bytes.extend_from_slice(&[0xff, 0xfe]);
        bytes.extend_from_slice(b"\0ACTION=online\0SUBSYSTEM=memory\0");

        assert!(is_memory_hotplug(&bytes));
    }
}
