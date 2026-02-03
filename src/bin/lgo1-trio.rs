use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use dbus::arg as dbus_arg;
use dbus_crossroads::Crossroads;
use evdev::{AttributeSet, BusType, EventType, InputEvent, KeyCode, SwitchCode};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
/// A changed_properties array suitable for a PropertiesChanged signal.
/// See https://github.com/diwic/dbus-rs/blob/master/dbus/examples/argument_guide.md.
type ChangedProps<'a> = Vec<(&'a str, dbus_arg::Variant<Box<dyn dbus_arg::RefArg>>)>;
type ChangedPropsQueue<'a> = VecDeque<ChangedProps<'a>>;

enum KeyboardStatus {
    /// The keyboard case is connected.
    CaseExternal = 0x2,
    /// Any external keyboard, excluding the keyboard case, is connected.
    AnyExternal = 0x1,
    /// No external keyboard is connected.
    None = 0x0,
}

struct DBusObject {
    keyboard_status: u32,
}

const DBUS_OBJECT_PATH: &str = "/com/youngryan/LGo1Trio";
const DBUS_INTERFACE: &str = "com.youngryan.LGo1Trio";

fn main() {
    spawn_loop("run_virtual_device", run_virtual_device);

    let (udev_s, udev_r) = mpsc::sync_channel::<()>(0);
    // A reference or Arc is necessary to make the function callable multiple times.
    spawn_loop("read_udev_events", move || read_udev_events(&udev_s));

    let dbus_cr = Arc::new(Mutex::new(make_dbus_crossroads()));
    let dbus_cr2 = dbus_cr.clone();
    let cpq = Arc::new(Mutex::new(VecDeque::<ChangedProps>::new()));
    let cpq2 = cpq.clone();
    spawn_loop("read_keyboard_status", move || {
        read_keyboard_status(dbus_cr2.clone(), &udev_r, cpq2.clone())
    });
    let _ = spawn_loop("run_dbus", move || run_dbus(dbus_cr.clone(), cpq.clone())).join();

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

fn run_virtual_device() -> Result<()> {
    const FORWARD_KEYS: [KeyCode; 2] = [KeyCode::KEY_VOLUMEDOWN, KeyCode::KEY_VOLUMEUP];
    let forward_codes: HashSet<u16> = FORWARD_KEYS.iter().map(|k| k.0).collect();

    let mut internal_keyboard = evdev::enumerate()
        .map(|(_, d)| d)
        .find(|d| {
            let id = d.input_id();
            id.bus_type() == BusType::BUS_I8042 && id.vendor() == 0x1 && id.product() == 0x1
        })
        .ok_or("could not find internal keyboard")?;

    let keys = AttributeSet::<KeyCode>::from_iter(FORWARD_KEYS.iter());
    let switches = AttributeSet::<SwitchCode>::from_iter([SwitchCode::SW_TABLET_MODE]);
    let mut device = evdev::uinput::VirtualDevice::builder()?
        .name("lgo1-trio virtual input device")
        .with_keys(&keys)?
        .with_switches(&switches)?
        .build()?;
    device.emit(&[evdev::InputEvent::new(
        EventType::SWITCH.0,
        SwitchCode::SW_TABLET_MODE.0,
        1,
    )])?;

    loop {
        for event in internal_keyboard.fetch_events()? {
            let code = event.code();
            if forward_codes.contains(&code) {
                device.emit(&[InputEvent::new(EventType::KEY.0, code, event.value())])?;
            }
        }
    }
}

fn make_dbus_crossroads() -> Crossroads {
    let mut cr = Crossroads::new();
    let iface_token = cr.register(
        DBUS_INTERFACE,
        |b: &mut dbus_crossroads::IfaceBuilder<DBusObject>| {
            b.property("KeyboardStatus")
                .get(|_, obj| Ok(obj.keyboard_status));
        },
    );
    cr.insert(
        DBUS_OBJECT_PATH,
        &[iface_token],
        DBusObject {
            keyboard_status: KeyboardStatus::None as u32,
        },
    );
    cr
}

fn read_udev_events(notify: &mpsc::SyncSender<()>) -> Result<()> {
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
                let _ = notify.try_send(());
            }
            _ => {}
        }
    }
}

fn read_keyboard_status(
    cr: Arc<Mutex<Crossroads>>,
    wait_for: &mpsc::Receiver<()>,
    signal: Arc<Mutex<ChangedPropsQueue>>,
) -> Result<()> {
    loop {
        // Wait for an update, but also force a recheck every now and then.
        match wait_for.recv_timeout(Duration::from_secs(120)) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            _ => {
                // Wait for all events to come in, and then impose a short delay. This
                // accounts for the time the kernel needs to add and remove devices.
                loop {
                    match wait_for.recv_timeout(Duration::from_millis(1000)) {
                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                        _ => continue,
                    }
                }
            }
        }

        let mut changes: ChangedProps = Vec::new();
        let status = keyboard_status() as u32;
        {
            let mut cr_lock = cr.lock().unwrap();
            let obj: &mut DBusObject = cr_lock.data_mut(&DBUS_OBJECT_PATH.into()).unwrap();

            if obj.keyboard_status != status {
                changes.push(("KeyboardStatus", dbus_arg::Variant(Box::new(status))));
                obj.keyboard_status = status;
            }
        }

        if changes.len() > 0 {
            let mut signal_lock = signal.lock().unwrap();
            signal_lock.push_back(changes);
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

fn run_dbus(cr: Arc<Mutex<Crossroads>>, to_send: Arc<Mutex<ChangedPropsQueue>>) -> Result<()> {
    use dbus::channel::{MatchingReceiver, Sender};

    let conn = dbus::blocking::LocalConnection::new_system()?;
    conn.request_name("com.youngryan.LGo1Trio", false, true, false)?;

    conn.start_receive(
        dbus::message::MatchRule::new_method_call(),
        Box::new(move |msg, conn| {
            let mut cr_lock = cr.lock().unwrap();
            let _ = cr_lock.handle_message(msg, conn);
            true
        }),
    );
    loop {
        conn.process(Duration::from_millis(100))?;
        {
            let mut to_send_lock = to_send.lock().unwrap();
            for changed_props in to_send_lock.drain(..) {
                conn.send(
                    dbus::Message::signal(
                        &DBUS_OBJECT_PATH.into(),
                        &"org.freedesktop.DBus.Properties".into(),
                        &"PropertiesChanged".into(),
                    )
                    .append3(
                        DBUS_INTERFACE,
                        changed_props,
                        dbus_arg::Array::new(std::iter::empty::<&str>()),
                    ),
                )
                .map_err(|_| "failed to send properties changed message")?;
            }
        }
    }
}
