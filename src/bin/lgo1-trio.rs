use std::collections::HashSet;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use evdev::{AttributeSet, BusType, InputEvent, KeyCode, SwitchCode};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Copy, Debug, PartialEq)]
enum KeyboardStatus {
    /// The keyboard case is connected.
    CaseExternal = 0x2,
    /// Any external keyboard, excluding the keyboard case, is connected.
    AnyExternal = 0x1,
    /// No external keyboard is connected.
    None = 0x0,
}

impl KeyboardStatus {
    fn load_atomic(atomic: &AtomicU32) -> KeyboardStatus {
        match atomic.load(Ordering::Relaxed) {
            0x2 => KeyboardStatus::CaseExternal,
            0x1 => KeyboardStatus::AnyExternal,
            0x0 | _ => KeyboardStatus::None,
        }
    }

    fn store_atomic(&self, atomic: &AtomicU32) {
        atomic.store(*self as u32, Ordering::Relaxed)
    }

    fn is_tablet_mode(&self) -> bool {
        *self == KeyboardStatus::None
    }
}

const FORWARD_KEYS: [KeyCode; 2] = [KeyCode::KEY_VOLUMEDOWN, KeyCode::KEY_VOLUMEUP];

fn main() {
    let atomic_status = Arc::new(AtomicU32::new(KeyboardStatus::None as u32));
    let atomic_status2 = atomic_status.clone();

    // (We pass references and Arc clones make the functions callable multiple
    // times.)

    let (virtual_s, virtual_r) = mpsc::channel::<InputEvent>();
    let virtual_s2 = virtual_s.clone();
    spawn_loop("read_suppressed_keyboard", move || {
        read_suppressed_keyboard(&virtual_s)
    });
    spawn_loop("run_virtual_device", move || {
        run_virtual_device(&virtual_r, atomic_status2.clone())
    });

    let (udev_s, udev_r) = mpsc::sync_channel::<()>(0);
    spawn_loop("read_udev_add_remove", move || {
        read_udev_add_remove(&udev_s)
    });
    let _ = spawn_loop("read_keyboard_status", move || {
        read_keyboard_status(&udev_r, &virtual_s2, atomic_status.clone())
    })
    .join();

    unreachable!();
}

/// Spawn a new thread in an infinite loop with error reporting.
fn spawn_loop<F, T>(name: &'static str, mut f: F) -> thread::JoinHandle<T>
where
    F: FnMut() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    thread::spawn(move || {
        loop {
            match f() {
                Ok(_) => {}
                Err(err) => eprintln!("Error in {}: {}", name, err),
            }
            thread::sleep(Duration::from_secs(10));
        }
    })
}

fn read_suppressed_keyboard(consumer: &mpsc::Sender<InputEvent>) -> Result<()> {
    let forward_codes: HashSet<u16> = FORWARD_KEYS.iter().map(|k| k.0).collect();

    let mut internal_keyboard = evdev::enumerate()
        .map(|(_, d)| d)
        .find(|d| {
            let id = d.input_id();
            id.bus_type() == BusType::BUS_I8042 && id.vendor() == 0x1 && id.product() == 0x1
        })
        .ok_or("could not find internal keyboard")?;

    loop {
        for event in internal_keyboard.fetch_events()? {
            let code = event.code();
            if forward_codes.contains(&code) {
                consumer.send(InputEvent::new(
                    evdev::EventType::KEY.0,
                    code,
                    event.value(),
                ))?;
            }
        }
    }
}

fn run_virtual_device(
    event_stream: &mpsc::Receiver<InputEvent>,
    atomic_status: Arc<AtomicU32>,
) -> Result<()> {
    let keys = AttributeSet::<KeyCode>::from_iter(FORWARD_KEYS.iter());
    let switches = AttributeSet::<SwitchCode>::from_iter([SwitchCode::SW_TABLET_MODE]);
    let mut device = evdev::uinput::VirtualDevice::builder()?
        .name("lgo1-trio virtual input device")
        .with_keys(&keys)?
        .with_switches(&switches)?
        .build()?;

    loop {
        let event = event_stream.recv()?;
        if KeyboardStatus::load_atomic(&atomic_status).is_tablet_mode() {
            device.emit(&[event])?;
        }
    }
}

fn read_udev_add_remove(consumer: &mpsc::SyncSender<()>) -> Result<()> {
    use std::os::unix::io::AsRawFd;

    let socket = udev::MonitorBuilder::new()?
        .match_subsystem("input")?
        .listen()?;

    let mut fds = vec![libc::pollfd {
        fd: socket.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    }];

    loop {
        let result = unsafe {
            libc::ppoll(
                (&mut fds[..]).as_mut_ptr(),
                fds.len() as libc::nfds_t,
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if result < 0 {
            return Err(From::from(std::io::Error::last_os_error()));
        }
        let event = match socket.iter().next() {
            Some(evt) => evt,
            None => {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
        };
        match event.event_type() {
            udev::EventType::Add | udev::EventType::Remove => {
                let _ = consumer.try_send(());
            }
            _ => {}
        }
    }
}

fn read_keyboard_status(
    udev_add_remove: &mpsc::Receiver<()>,
    virtual_consumer: &mpsc::Sender<InputEvent>,
    atomic_status: Arc<AtomicU32>,
) -> Result<()> {
    loop {
        let status = keyboard_status();
        virtual_consumer.send(InputEvent::new(
            evdev::EventType::SWITCH.0,
            SwitchCode::SW_TABLET_MODE.0,
            status.is_tablet_mode() as i32,
        ))?;
        status.store_atomic(&atomic_status);

        // Wait for an update, but also force a recheck every now and then.
        match udev_add_remove.recv_timeout(Duration::from_secs(120)) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            _ => {
                // Wait for all events to come in, and then impose a short delay. This
                // accounts for the time the kernel needs to add and remove devices.
                loop {
                    match udev_add_remove.recv_timeout(Duration::from_millis(1000)) {
                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                        _ => continue,
                    }
                }
            }
        }
    }
}

fn keyboard_status() -> KeyboardStatus {
    const TEST_KEYS: [KeyCode; 3] = [KeyCode::KEY_ENTER, KeyCode::KEY_BACKSPACE, KeyCode::KEY_ESC];
    const INTERNAL_BLACKLIST: [(BusType, u16, u16); 2] = [
        (BusType::BUS_I8042, 0x1, 0x1),     // AT Translated Set 2 keyboard
        (BusType::BUS_USB, 0x17ef, 0x6184), // Legion-Controller 1-B0 Keyboard
    ];
    let internal_blacklist: HashSet<(u16, u16, u16)> = INTERNAL_BLACKLIST
        .iter()
        .map(|&(bus_type, vendor, product)| (bus_type.0, vendor, product))
        .collect();

    for d in evdev::enumerate().map(|(_, d)| d) {
        let id = d.input_id();
        let id_t = (id.bus_type().0, id.vendor(), id.product());
        if id_t == (BusType::BUS_BLUETOOTH.0, 0x04e8, 0x7021) {
            return KeyboardStatus::CaseExternal;
        }

        let looks_like_keyboard = d.supported_keys().map_or(false, |attr_set| {
            TEST_KEYS.iter().all(|&k| attr_set.contains(k))
        });
        let is_blacklisted = internal_blacklist.contains(&id_t);
        if looks_like_keyboard && !is_blacklisted {
            return KeyboardStatus::AnyExternal;
        }
    }
    KeyboardStatus::None
}
