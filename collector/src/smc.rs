use std::ffi::{c_char, c_void};
use std::io;

use crate::kernel::{self, KernReturn, MachPort};

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingService(main_port: MachPort, matching: *mut c_void) -> MachPort;
    fn IOServiceOpen(
        service: MachPort,
        owning_task: MachPort,
        kind: u32,
        connection: *mut MachPort,
    ) -> KernReturn;
    fn IOServiceClose(connection: MachPort) -> KernReturn;
    fn IOObjectRelease(object: MachPort) -> KernReturn;
    fn IOConnectCallStructMethod(
        connection: MachPort,
        selector: u32,
        input: *const c_void,
        input_size: usize,
        output: *mut c_void,
        output_size: *mut usize,
    ) -> KernReturn;
}

const CALL_STRUCT: u32 = 2;
const READ_BYTES: u8 = 5;
const READ_INDEX: u8 = 8;
const READ_KEY_INFO: u8 = 9;

const KEY_COUNT: Key = Key::named(*b"#KEY");
const FAN_COUNT: Key = Key::named(*b"FNum");
const SYSTEM_POWER: Key = Key::named(*b"PSTR");
const SOC_TEMPERATURE_PREFIXES: [[u8; 2]; 8] = [
    *b"TC", *b"Tp", *b"Tf", *b"Ts", *b"Te", *b"TR", *b"Tg", *b"TP",
];

const FLOAT: KeyInfo = KeyInfo::of(*b"flt ", 4);
const UNSIGNED_8: KeyInfo = KeyInfo::of(*b"ui8 ", 1);
const UNSIGNED_32: KeyInfo = KeyInfo::of(*b"ui32", 4);

#[derive(Clone, Copy, PartialEq, Eq)]
struct Key(u32);

impl Key {
    const fn named(name: [u8; 4]) -> Self {
        Key(u32::from_be_bytes(name))
    }

    fn fan_speed(fan: u8) -> Self {
        Key::named([b'F', b'0' + fan, b'A', b'c'])
    }

    fn is_soc_temperature(self) -> bool {
        let [first, second, _, _] = self.0.to_be_bytes();
        SOC_TEMPERATURE_PREFIXES.contains(&[first, second])
    }
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&String::from_utf8_lossy(&self.0.to_be_bytes()))
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct KeyInfo {
    size: u32,
    kind: u32,
    _attributes: u8,
}

impl KeyInfo {
    const fn of(kind: [u8; 4], size: u32) -> Self {
        Self {
            size,
            kind: u32::from_be_bytes(kind),
            _attributes: 0,
        }
    }

    fn is(self, other: KeyInfo) -> bool {
        self.size == other.size && self.kind == other.kind
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Message {
    key: u32,
    _version_and_limits: [u8; 24],
    info: KeyInfo,
    result: u8,
    _status: u8,
    command: u8,
    index: u32,
    bytes: [u8; 32],
}

const _: () = assert!(size_of::<Message>() == 80);
const _: () = assert!(std::mem::offset_of!(Message, info) == 28);
const _: () = assert!(std::mem::offset_of!(Message, result) == 40);
const _: () = assert!(std::mem::offset_of!(Message, index) == 44);
const _: () = assert!(std::mem::offset_of!(Message, bytes) == 48);

pub struct Smc {
    connection: Connection,
    fans: Vec<Key>,
    soc_sensor: Key,
    pub watts: f32,
    pub fan_rpm: Vec<f32>,
    pub soc_celsius: f32,
}

impl Smc {
    pub fn open() -> io::Result<Self> {
        let connection = Connection::open()?;
        let fans = (0..connection.read(FAN_COUNT, UNSIGNED_8)?[0])
            .map(Key::fan_speed)
            .collect();
        let soc_sensor = connection.hottest_soc_sensor()?;

        Ok(Self {
            connection,
            fans,
            soc_sensor,
            watts: 0.0,
            fan_rpm: Vec::new(),
            soc_celsius: 0.0,
        })
    }

    pub fn read(&mut self) -> io::Result<()> {
        self.watts = self.connection.read_float(SYSTEM_POWER)?;
        self.fan_rpm.clear();
        for &fan in &self.fans {
            self.fan_rpm.push(self.connection.read_float(fan)?);
        }
        self.soc_celsius = self.connection.read_float(self.soc_sensor)?;

        Ok(())
    }
}

struct Connection(MachPort);

impl Drop for Connection {
    fn drop(&mut self) {
        unsafe { IOServiceClose(self.0) };
    }
}

impl Connection {
    fn open() -> io::Result<Self> {
        let service = unsafe {
            IOServiceGetMatchingService(0, IOServiceMatching(c"AppleSMCKeysEndpoint".as_ptr()))
        };
        if service == 0 {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "found no SMC to connect to",
            ));
        }

        let mut connection = 0;
        let opened = unsafe { IOServiceOpen(service, kernel::task_self(), 0, &raw mut connection) };
        unsafe { IOObjectRelease(service) };
        if opened != kernel::KERN_SUCCESS {
            return Err(io::Error::other(format!(
                "connecting to the SMC returned {opened:#x}"
            )));
        }

        Ok(Self(connection))
    }

    fn hottest_soc_sensor(&self) -> io::Result<Key> {
        let mut readings = Vec::new();
        for index in 0..self.key_count()? {
            let Some(key) = self.key_at(index)? else {
                continue;
            };
            if !key.is_soc_temperature() || !self.info(key)?.is_some_and(|info| info.is(FLOAT)) {
                continue;
            }
            let Some(bytes) = self.value(key, FLOAT)? else {
                continue;
            };

            readings.push((key, float_from(bytes)));
        }

        hottest(readings).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "the SMC has no SoC temperature sensor",
            )
        })
    }

    fn key_count(&self) -> io::Result<u32> {
        let [a, b, c, d, ..] = self.read(KEY_COUNT, UNSIGNED_32)?;
        Ok(u32::from_be_bytes([a, b, c, d]))
    }

    fn key_at(&self, index: u32) -> io::Result<Option<Key>> {
        let listed = self.ask(Message {
            command: READ_INDEX,
            index,
            ..Message::default()
        })?;
        Ok(listed.map(|listed| Key(listed.key)))
    }

    fn info(&self, key: Key) -> io::Result<Option<KeyInfo>> {
        let described = self.ask(Message {
            key: key.0,
            command: READ_KEY_INFO,
            ..Message::default()
        })?;
        Ok(described.map(|described| described.info))
    }

    fn read_float(&self, key: Key) -> io::Result<f32> {
        self.read(key, FLOAT).map(float_from)
    }

    fn read(&self, key: Key, info: KeyInfo) -> io::Result<[u8; 32]> {
        self.value(key, info)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("the SMC has no value for {key:?}"),
            )
        })
    }

    fn value(&self, key: Key, info: KeyInfo) -> io::Result<Option<[u8; 32]>> {
        let answer = self.ask(Message {
            key: key.0,
            info,
            command: READ_BYTES,
            ..Message::default()
        })?;
        Ok(answer.map(|answer| answer.bytes))
    }

    fn ask(&self, question: Message) -> io::Result<Option<Message>> {
        let mut answer = Message::default();
        let mut size = size_of::<Message>();
        let returned = unsafe {
            IOConnectCallStructMethod(
                self.0,
                CALL_STRUCT,
                (&raw const question).cast(),
                size_of::<Message>(),
                (&raw mut answer).cast(),
                &raw mut size,
            )
        };
        if returned != kernel::KERN_SUCCESS {
            return Err(io::Error::other(format!(
                "calling the SMC returned {returned:#x}"
            )));
        }

        Ok((answer.result == 0).then_some(answer))
    }
}

fn hottest(readings: impl IntoIterator<Item = (Key, f32)>) -> Option<Key> {
    readings
        .into_iter()
        .max_by(|(_, one), (_, other)| one.total_cmp(other))
        .map(|(key, _)| key)
}

fn float_from([a, b, c, d, ..]: [u8; 32]) -> f32 {
    f32::from_ne_bytes([a, b, c, d])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_hottest_of_the_sensors_read() {
        let readings = [
            (Key::named(*b"Tp01"), 41.0),
            (Key::named(*b"Tg05"), 63.5),
            (Key::named(*b"TP0a"), 52.0),
        ];

        assert_eq!(Some(Key::named(*b"Tg05")), hottest(readings));
    }

    #[test]
    fn reads_the_power_the_mac_draws() {
        // Goal: the SMC's system power reads as a draw a running Mac can have.
        // Method: one read, held above zero and below 500 W. An idle M5 Pro on AC power has
        // read 0.73 W, so the floor is zero.
        let mut smc = Smc::open().unwrap();

        smc.read().unwrap();

        assert!(smc.watts > 0.0, "{} W", smc.watts);
        assert!(smc.watts < 500.0, "{} W", smc.watts);
    }

    #[test]
    fn reads_a_speed_for_every_fan() {
        let mut smc = Smc::open().unwrap();
        let fans = Connection::open()
            .unwrap()
            .read(FAN_COUNT, UNSIGNED_8)
            .unwrap()[0];

        smc.read().unwrap();

        assert_eq!(usize::from(fans), smc.fan_rpm.len());
    }

    #[test]
    fn reads_each_fan_speed_in_rpm() {
        let mut smc = Smc::open().unwrap();

        smc.read().unwrap();

        assert!(
            smc.fan_rpm
                .iter()
                .all(|&rpm| (0.0..10_000.0).contains(&rpm)),
            "{:?}",
            smc.fan_rpm
        );
    }

    #[test]
    fn reads_the_temperature_of_a_soc_sensor() {
        let mut smc = Smc::open().unwrap();

        smc.read().unwrap();

        assert!(
            smc.soc_celsius > 15.0 && smc.soc_celsius < 115.0,
            "{} °C",
            smc.soc_celsius
        );
    }
}
