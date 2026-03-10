#![no_std]
#![no_main]
#![feature(ascii_char)]
#![feature(sync_unsafe_cell)]
#![feature(int_lowest_highest_one)]

mod alloc;
mod fmt;
mod log;
mod trace;

use core::{
    cell::UnsafeCell,
    fmt::Write,
    hint::black_box,
    ops::{Deref, DerefMut},
    panic::PanicInfo,
    sync::atomic::AtomicUsize,
};

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_sync::{
    blocking_mutex::{
        ThreadModeMutex,
        raw::{NoopRawMutex, RawMutex, ThreadModeRawMutex},
    },
    channel::{Channel, Sender},
    mutex::Mutex,
    once_lock::OnceLock,
};
use embassy_usb::{
    Builder,
    class::{
        cdc_acm::{CdcAcmClass, State},
        hid::{self, RequestHandler},
    },
    driver::EndpointError,
};
//#[cfg(not(feature = "defmt"))]
//use panic_halt as _;
#[cfg(feature = "defmt")]
use defmt_rtt as _;
use embedded_hal::i2c::Operation;
use sync_unsafe_cell::SyncUnsafeCell;
use usbd_hid::descriptor::gen_hid_descriptor;

use embassy_executor::Spawner;
use embassy_futures::join::{join, join_array, join3, join4, join5};
use embassy_stm32::{
    adc::{self, Adc, AdcChannel, AnyAdcChannel},
    bind_interrupts, dma,
    gpio::{Input as GpioInput, Level, Output, Speed},
    i2c::{self, I2c, Master},
    mode::Async,
    peripherals::{self, ADC1, DMA1_CH4, PA8, PB8, USB},
    rcc::mux::ClockMux,
    time::Hertz,
    usb::{self, Driver, Instance},
};
use embassy_time::{Duration, Instant, Ticker, Timer};
//use fmt::info;
use embedded_hal_async::i2c::I2c as _;

use crate::{
    alloc::BumpAllocator,
    log::{info, log_receiver},
    trace::{reset_profile, set_task_name, take_profile},
};

bind_interrupts!(struct Irqs {
    USB => usb::InterruptHandler<peripherals::USB>;
    ADC1_COMP => adc::InterruptHandler<peripherals::ADC1>;
    I2C2 => i2c::EventInterruptHandler<peripherals::I2C2>, i2c::ErrorInterruptHandler<peripherals::I2C2>;
    DMA1_CHANNEL4_5_6_7 => dma::InterruptHandler<peripherals::DMA1_CH4>, dma::InterruptHandler<peripherals::DMA1_CH5>;
});

static PANIC_COUNT: AtomicUsize = AtomicUsize::new(0);

fn set_panic_count(cnt: usize) {
    PANIC_COUNT.store(cnt, core::sync::atomic::Ordering::SeqCst);
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Safety: We've panicked
    let pb8 = unsafe { PB8::steal() };
    let mut led = Output::new(pb8, Level::Low, Speed::Low);

    loop {
        let count = PANIC_COUNT.load(core::sync::atomic::Ordering::SeqCst);
        for _ in 0..count {
            led.set_high();
            for _ in 0..2_000_000 {
                black_box(());
            }
            led.set_low();
            for _ in 0..2_000_000 {
                black_box(());
            }
        }

        led.set_low();
        for _ in 0..10_000_000 {
            black_box(());
        }
    }
}

struct Disconnected {}

impl From<EndpointError> for Disconnected {
    fn from(val: EndpointError) -> Self {
        match val {
            EndpointError::BufferOverflow => panic!("Buffer overflow"),
            EndpointError::Disabled => Disconnected {},
        }
    }
}

static ALLOCATOR: BumpAllocator<4096> = BumpAllocator::new();

/*struct PoolAllocator<M, T, const N: usize> {
    pool: [SyncUnsafeCell<T>; N],
    free: SyncUnsafeCell<[u8; N]>,
    mutex: M,
}

struct PoolAllocatorGuard<'a, M: RawMutex, T, const N: usize> {
    pool: &'a PoolAllocator<M, T, N>,
    //target: &'a mut T,
    index: usize,
}

impl<'a, M: RawMutex, T, const N: usize> Deref for PoolAllocatorGuard<'a, M, T, N> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*(self.pool.pool[self.index].get() as *const T) }
    }
}

impl<'a, M: RawMutex, T, const N: usize> DerefMut for PoolAllocatorGuard<'a, M, T, N> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *(self.pool.pool[self.index].get()) }
    }
}

impl<'a, M: RawMutex, T, const N: usize> Drop for PoolAllocatorGuard<'a, M, T, N> {
    fn drop(&mut self) {
        self.pool.mutex.lock(|| unsafe {
            let free = unsafe { &mut *self.pool.free.get() };
            free[self.index] = 1;
        });
    }
}

impl<M: RawMutex, T: Copy, const N: usize> PoolAllocator<M, T, N> {
    const fn new(init_array: [SyncUnsafeCell<T>; N]) -> Self {
        Self {
            pool: init_array,
            free: SyncUnsafeCell::new([1; N]),
            mutex: M::INIT,
        }
    }
}

impl<M: RawMutex, T, const N: usize> PoolAllocator<M, T, N> {
    fn allocate<'a>(&'a self) -> Option<PoolAllocatorGuard<'a, M, T, N>> {
        self.mutex.lock(|| {
            let free = unsafe { &mut *self.free.get() };
            for idx in 0..N {
                if free[idx] == 1 {
                    free[idx] = 0;
                    return Some(PoolAllocatorGuard {
                        pool: self, //unsafe { &mut *(((self) as *const Self) as *mut Self) },
                        index: idx,
                    });
                }
            }
            None
        })
    }
}

// TODO: Can we get rid of Copy?
#[derive(Clone, Copy)]
struct PendingMessage {
    message: [u8; 256],
    len: usize,
}

impl PendingMessage {
    const fn new() -> Self {
        Self {
            message: [0; 256],
            len: 0,
        }
    }

    fn reset(&mut self) {
        self.len = 0;
    }

    fn push_char(&mut self, c: u8) {
        if self.len < self.message.len() {
            self.message[self.len] = c;
            self.len += 1;
        }
    }
}

impl core::fmt::Write for PendingMessage {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for c in s.chars() {
            if c == '\n' {
                self.push_char(b'\r');
                self.push_char(b'\n');
            } else if let Some(ascii) = c.as_ascii() {
                self.push_char(ascii.to_u8());
            } else {
                self.push_char(b'?');
            }
        }
        core::fmt::Result::Ok(())
    }
}

static UART_MESSAGE_POOL: PoolAllocator<ThreadModeRawMutex, PendingMessage, 3> =
    PoolAllocator::new([const { SyncUnsafeCell::new(PendingMessage::new()) }; 3]);

/*static UART_SENDER: OnceLock<
    Sender<ThreadModeRawMutex, PoolAllocatorGuard<'static, PendingMessage>, 4>,
> = OnceLock::new();*/

static UART_CHANNEL: Channel<
    ThreadModeRawMutex,
    PoolAllocatorGuard<'static, ThreadModeRawMutex, PendingMessage, 3>,
    3,
> = Channel::<ThreadModeRawMutex, _, _>::new();

macro_rules! info {
    ($s:literal $(, $x:expr)* $(,)?) => {
        {
            let args = core::format_args!($s $(, $x)*);

            if let Some(mut message) = UART_MESSAGE_POOL.allocate() {
                message.reset();
                core::fmt::write(&mut *message, args);
                message.write_str("\n");
                /*message.len = 6;
                message.message[0] = b'h';
                message.message[1] = b'e';
                message.message[2] = b'l';
                message.message[3] = b'l';
                message.message[4] = b'o';
                message.message[5] = b'\n';*/

                UART_CHANNEL.sender().send(message).await;
            }

            /*if let Some(message) = $out.try_send() {
                message.len = 6;
                message.message[0] = b'h';
            }*/

            /*#[cfg(feature = "defmt")]
            ::defmt::info!($s $(, $x)*);
            #[cfg(not(feature="defmt"))]
            let _ = ($( & $x ),*);*/
        }
    };
}*/

/*async fn echo<'d, T: Instance + 'd>(
    class: &mut CdcAcmClass<'d, Driver<'d, T>>,
) -> Result<(), Disconnected> {
    let mut buf = [0; 64];
    loop {
        let n = class.read_packet(&mut buf).await?;
        let data = &buf[..n];
        info!("data: {:x}", data);

        let mut buf = [0; 256];
        let mut offset = 0;
        for c in b"Hello from USB land! You said: '" {
            buf[offset] = *c;
            offset += 1;
        }
        for c in data.iter() {
            buf[offset] = *c;
            offset += 1;
        }
        for c in b"'\r\n" {
            buf[offset] = *c;
            offset += 1;
        }
        let buf = &buf[0..offset];
        class.write_packet(buf).await?;
    }
}*/

struct Input {
    constant: bool,
    variable: bool,
    relative: bool,
    wrap: bool,
    non_linear: bool,
    no_preferred_state: bool,
    null_state: bool,
    buffered_bytes: bool,
}

impl Input {
    const fn make_payload(&self) -> ShortDataPayload {
        let mut res = 0;
        if self.constant {
            res |= 1 << 0;
        }
        if self.variable {
            res |= 1 << 1;
        }
        if self.relative {
            res |= 1 << 2;
        }
        if self.wrap {
            res |= 1 << 3;
        }
        if self.non_linear {
            res |= 1 << 4;
        }
        if self.no_preferred_state {
            res |= 1 << 5;
        }
        if self.null_state {
            res |= 1 << 6;
        }
        if self.buffered_bytes {
            res |= 1 << 8;
        }
        ShortDataPayload::U32(res)
    }
}

enum Collection {
    Physical,
    Application,
    Logical,
    Report,
    NamedArray,
    UsageSwitch,
    UsageModifier,
}

impl Collection {
    const fn tag(&self) -> u8 {
        match self {
            Self::Physical => 0,
            Self::Application => 1,
            Self::Logical => 2,
            Self::Report => 3,
            Self::NamedArray => 4,
            Self::UsageSwitch => 5,
            Self::UsageModifier => 6,
        }
    }
}

enum Item {
    // Main items
    Input(Input),
    Output,
    Feature,
    Collection(Collection),
    EndCollection,

    // Global items
    UsagePage,
    LogicalMinimum(u16),
    LogicalMaximum(u16),
    PhysicalMinimum,
    PhysicalMaximum,
    UnitExponent,
    Unit,
    ReportSize(u8),
    ReportId,
    ReportCount(u8),
    Push,
    Pop,

    // Local items
    Usage,
    UsageMinimum,
    UsageMaximum,
    DesignatorIndex,
    DesignatorMinimum,
    DesignatorMaximum,
    StringIndex,
    StringMinimum,
    StringMaximum,
    Delimiter,
}

enum ShortDataPayload {
    None,
    U8(u8),
    U16(u16),
    U32(u32),
}

impl ShortDataPayload {
    const fn len(&self) -> u8 {
        match self {
            Self::None => 0,
            Self::U8(_) => 1,
            Self::U16(_) => 2,
            Self::U32(_) => 3,
        }
    }
}

impl Item {
    const fn btype(&self) -> u8 {
        match self {
            Self::Input(_)
            | Self::Output
            | Self::Feature
            | Self::Collection(_)
            | Self::EndCollection => 0,
            Self::UsagePage
            | Self::LogicalMaximum(_)
            | Self::LogicalMinimum(_)
            | Self::PhysicalMinimum
            | Self::PhysicalMaximum
            | Self::UnitExponent
            | Self::Unit
            | Self::ReportSize(_)
            | Self::ReportId
            | Self::ReportCount(_)
            | Self::Push
            | Self::Pop => 1,
            Self::Usage
            | Self::UsageMinimum
            | Self::UsageMaximum
            | Self::DesignatorIndex
            | Self::DesignatorMinimum
            | Self::DesignatorMaximum
            | Self::StringIndex
            | Self::StringMinimum
            | Self::StringMaximum
            | Self::Delimiter => 2,
        }
    }

    const fn btag(&self) -> u8 {
        match self {
            Self::Input(_) => 8,
            Self::Output => 9,
            Self::Feature => 0b1011,
            Self::Collection(_) => 0b1010,
            Self::EndCollection => 0b1100,

            Self::UsagePage => 0,
            Self::LogicalMinimum(_) => 1,
            Self::LogicalMaximum(_) => 2,
            Self::PhysicalMinimum => 3,
            Self::PhysicalMaximum => 4,
            Self::UnitExponent => 5,
            Self::Unit => 6,
            Self::ReportSize(_) => 7,
            Self::ReportId => 8,
            Self::ReportCount(_) => 9,
            Self::Push => 0b1010,
            Self::Pop => 0b1011,

            Self::Usage => 0,
            Self::UsageMinimum => 1,
            Self::UsageMaximum => 2,
            Self::DesignatorIndex => 3,
            Self::DesignatorMinimum => 4,
            Self::DesignatorMaximum => 5,
            Self::StringIndex => 7,
            Self::StringMinimum => 8,
            Self::StringMaximum => 9,
            Self::Delimiter => 10,
        }
    }

    const fn serialize_data(&self) -> ShortDataPayload {
        match self {
            Self::Input(input) => input.make_payload(),
            Self::Collection(collection) => ShortDataPayload::U8(collection.tag()),
            Self::EndCollection => ShortDataPayload::None,
            Self::LogicalMinimum(minimum) => ShortDataPayload::U16(*minimum),
            Self::LogicalMaximum(maximum) => ShortDataPayload::U16(*maximum),
            Self::ReportSize(v) | Self::ReportCount(v) => ShortDataPayload::U8(*v),
            _ => todo!(),
        }
    }
}

struct ReportDescriptorWriter<'a> {
    buf: &'a mut [u8],
    offset: usize,
}

impl<'a> ReportDescriptorWriter<'a> {
    const fn item(&mut self, item: Item) {
        let ty_ = item.btype();
        let tag = item.btag();
        let data = item.serialize_data();
        let datalen = data.len();
        self.buf[self.offset] = (tag << 4) | (ty_ << 2) | datalen;
        self.offset += 1;
        match data {
            ShortDataPayload::None => {}
            ShortDataPayload::U8(b) => {
                self.buf[self.offset] = b;
                self.offset += 1;
            }
            ShortDataPayload::U16(b) => {
                self.buf[self.offset] = (b & 0xff) as u8;
                self.buf[self.offset + 1] = (b >> 8) as u8;
                self.offset += 2;
            }
            ShortDataPayload::U32(b) => {
                self.buf[self.offset] = (b & 0xff) as u8;
                self.buf[self.offset + 1] = ((b >> 8) & 0xff) as u8;
                self.buf[self.offset + 2] = ((b >> 16) & 0xff) as u8;
                self.buf[self.offset + 3] = ((b >> 24) & 0xff) as u8;
                self.offset += 4;
            }
        }
    }
}

const fn build_report_descriptor(buf: &mut [u8]) -> usize {
    let mut writer = ReportDescriptorWriter { buf, offset: 0 };
    writer.item(Item::Collection(Collection::Application));

    writer.item(Item::ReportCount(1));
    writer.item(Item::ReportSize(1));
    writer.item(Item::LogicalMinimum(0));
    writer.item(Item::LogicalMaximum(1));
    writer.item(Item::Input(Input {
        constant: false,
        variable: true,
        relative: false,
        wrap: false,
        non_linear: false,
        no_preferred_state: false,
        null_state: false,
        buffered_bytes: false,
    }));

    writer.item(Item::EndCollection);
    writer.offset
}

use usbd_hid::descriptor::AsInputReport;
use usbd_hid::descriptor::SerializedDescriptor;
use usbd_hid::descriptor::generator_prelude::*;

/*
CH PRO PEDALS:
0x05, 0x01,        // Usage Page (Generic Desktop Ctrls)
0x09, 0x04,        // Usage (Joystick)
0xA1, 0x01,        // Collection (Application)
0x05, 0x01,        //   Usage Page (Generic Desktop Ctrls)
0x09, 0x01,        //   Usage (Pointer)
0xA1, 0x00,        //   Collection (Physical)
0x09, 0x30,        //     Usage (X)
0x09, 0x31,        //     Usage (Y)
0x09, 0x32,        //     Usage (Z)
0x15, 0x00,        //     Logical Minimum (0)
0x26, 0xFF, 0x00,  //     Logical Maximum (255)
0x75, 0x08,        //     Report Size (8)
0x95, 0x03,        //     Report Count (3)
0x81, 0x02,        //     Input (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position)
0xC0,              //   End Collection
0xC0,              // End Collection

This:
0x05, 0x01,        // Usage Page (Generic Desktop Ctrls)
0x09, 0x04,        // Usage (Joystick)
0xA1, 0x01,        // Collection (Application)
0x05, 0x01,        //   Usage Page (Generic Desktop Ctrls)
0x09, 0x01,        //   Usage (Pointer)
0x19, 0x30,        //   Usage Minimum (X)
0x29, 0x31,        //   Usage Maximum (Y)
0xA1, 0x00,        //   Collection (Physical)
0x17, 0x81, 0xFF, 0xFF, 0xFF,  //     Logical Minimum (-128)
0x25, 0x7F,        //     Logical Maximum (127)
0x75, 0x08,        //     Report Size (8)
0x95, 0x01,        //     Report Count (1)
0x81, 0x02,        //     Input (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position)
0x81, 0x02,        //     Input (Data,Var,Abs,No Wrap,Linear,Preferred State,No Null Position)
0xC0,              //   End Collection
0xC0,              // End Collection
*/

#[gen_hid_descriptor(
    (collection = APPLICATION, usage_page = GENERIC_DESKTOP, usage = JOYSTICK) = {
        /*(collection = PHYSICAL, usage_page = BUTTON, usage_min = 1, usage_max = 9) = {
            #[packed_bits 8, variable,absolute] buttons0=input;
        };*/
        (collection = PHYSICAL, usage_page = GENERIC_DESKTOP, usage = POINTER) = {
            (usage = 0x30, usage = 0x30) = {
                #[item_settings data,variable,absolute] axis0=input;
            };
            (usage = 0x31, usage = 0x31) = {
                #[item_settings data,variable,absolute] axis1=input;
            };
            (usage = 0x32, usage = 0x32) = {
                #[item_settings data,variable,absolute] axis2=input;
            };
            (usage = 0x33, usage = 0x33) = {
                #[item_settings data,variable,absolute] axis3=input;
            };
            (usage = 0x34, usage = 0x34) = {
                #[item_settings data,variable,absolute] axis4=input;
            };
            (usage = 0x35, usage = 0x35) = {
                #[item_settings data,variable,absolute] axis5=input;
            };
            (usage = 0x36, usage = 0x36) = {
                #[item_settings data,variable,absolute] axis6=input;
            };
            (usage = 0x37, usage = 0x37) = {
                #[item_settings data,variable,absolute] axis7=input;
            };
        };
        (collection = PHYSICAL, usage_page = BUTTON) = {
            (usage_min = 1, usage_max = 8) = {
                #[packed_bits 8, variable,absolute] buttons0=input;
            };
            (usage_min = 9, usage_max = 16) = {
                #[packed_bits 8, variable,absolute] buttons1=input;
            };
            (usage_min = 17, usage_max = 24) = {
                #[packed_bits 8, variable,absolute] buttons2=input;
            };
            (usage_min = 25, usage_max = 32) = {
                #[packed_bits 8, variable,absolute] buttons3=input;
            };
            (usage_min = 33, usage_max = 42) = {
                #[packed_bits 8, variable,absolute] buttons4=input;
            };
            (usage_min = 41, usage_max = 48) = {
                #[packed_bits 8, variable,absolute] buttons5=input;
            };
            (usage_min = 49, usage_max = 56) = {
                #[packed_bits 8, variable,absolute] buttons6=input;
            };
            (usage_min = 57, usage_max = 64) = {
                #[packed_bits 8, variable,absolute] buttons7=input;
            };
        };
    }
)]
struct JoyReport {
    axis0: i8,
    axis1: i8,
    axis2: i8,
    axis3: i8,
    axis4: i8,
    axis5: i8,
    axis6: i8,
    axis7: i8,
    buttons0: u8,
    buttons1: u8,
    buttons2: u8,
    buttons3: u8,
    buttons4: u8,
    buttons5: u8,
    buttons6: u8,
    buttons7: u8,
}

struct JoyRequestHandler {}

impl RequestHandler for JoyRequestHandler {
    //
}

struct DeviceMapping {
    buttons: [usize; 16],

    /// (up, down)
    dual_throws: [(usize, usize); 14],

    /// (press, A, B)
    encoders: [(usize, usize, usize); 6],

    axes: [usize; 8],
}

impl DeviceMapping {
    const fn version1() -> Self {
        Self {
            buttons: [
                47, 46, 44, 45, 40, 41, 42, 43, 32, 33, 34, 35, 36, 37, 38, 39,
            ],
            dual_throws: [
                (3, 2),
                (5, 4),
                (7, 6),
                (9, 8),
                (11, 10),
                (13, 12),
                (15, 14),
                (1, 0),
                (28, 27),
                (30, 29),
                (26, 25),
                (20, 19),
                (22, 21),
                (24, 23),
            ],
            encoders: [
                (18, 17, 16),
                (53, 52, 51),
                (50, 49, 48),
                (56, 55, 54),
                (62, 61, 60),
                (59, 58, 57),
            ],
            axes: [3, 4, 1, 6, 2, 5, 0, 7],
        }
    }
}

static DEVICE_MAPPING: DeviceMapping = DeviceMapping::version1();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EncoderDirection {
    Increment,
    Decrement,
}

/// Tracks remaining pulses in a given direction
#[derive(Clone, Copy, Debug)]
struct PulseState {
    direction: EncoderDirection,
    count: usize,
}

impl PulseState {
    const fn new() -> Self {
        Self {
            direction: EncoderDirection::Decrement,
            count: 0,
        }
    }

    fn push(&mut self, direction: EncoderDirection) {
        if direction == self.direction {
            self.count += 1;
        } else if self.count == 0 {
            self.count = 1;
            self.direction = direction;
        } else {
            self.count -= 1;
        }
    }

    fn pop(&mut self) -> Option<EncoderDirection> {
        if self.count > 0 {
            self.count -= 1;
            Some(self.direction)
        } else {
            None
        }
    }
}

/// States while incrementing:
/// Idle --(1, 0)--> Ticking(Increment)
/// Idle --(0, 1)--> Ticking(Decrement)
/// Idle --(1, 1)--> AmbiguousMovement
/// Ticking --(0, 0)--> Idle
/// Ticking --(1, 1)--> Ticked (emit pulse!)
/// Ticking --(0, 1)--> Ticked (emit pulse!)
/// Ticked  --(0, 0)--> Idle
/// AmbiguousMovement --(0, 1)--> Ticked(Increment) (emit pulse!)
///
/// (decrementing is similar but with different EncoderDirection states)
#[derive(Clone, Copy)]
enum EncoderState {
    Idle,
    Ticking(EncoderDirection),
    Ticked(EncoderDirection),
    AmbiguousMovement,
}

impl EncoderState {
    const fn new() -> Self {
        Self::Idle
    }

    /// Returns Some(direction) if a pulse should be emitted
    fn update(&mut self, a: bool, b: bool) -> Option<EncoderDirection> {
        let (new_state, pulse) = match (*self, a, b) {
            (Self::Idle, true, false) => (Self::Ticking(EncoderDirection::Increment), None),
            (Self::Idle, false, true) => (Self::Ticking(EncoderDirection::Decrement), None),
            (Self::Idle, true, true) => (Self::AmbiguousMovement, None),

            (Self::Ticking(dir), false, false) => (Self::Idle, None),
            (Self::Ticking(EncoderDirection::Increment), _, true) => (
                Self::Ticked(EncoderDirection::Increment),
                Some(EncoderDirection::Increment),
            ),
            (Self::Ticking(EncoderDirection::Decrement), true, _) => (
                Self::Ticked(EncoderDirection::Decrement),
                Some(EncoderDirection::Decrement),
            ),

            (Self::Ticked(dir), false, false) => (Self::Idle, None),

            (Self::AmbiguousMovement, false, true) => (
                Self::Ticked(EncoderDirection::Increment),
                Some(EncoderDirection::Increment),
            ),
            (Self::AmbiguousMovement, true, false) => (
                Self::Ticked(EncoderDirection::Decrement),
                Some(EncoderDirection::Decrement),
            ),

            (state, _, _) => (state, None),
        };
        *self = new_state;
        pulse
    }
}

#[embassy_executor::task]
async fn led_task(mut led: Output<'static>) {
    loop {
        //info!("Hello, World!");
        led.set_high();
        Timer::after(Duration::from_millis(500)).await;
        led.set_low();
        Timer::after(Duration::from_millis(500)).await;
    }
}

#[embassy_executor::task]
async fn cli_task(
    mut uart_rx: embassy_usb::class::cdc_acm::Receiver<
        'static,
        embassy_stm32::usb::Driver<'static, USB>,
    >,
) {
    uart_rx.wait_connection().await;
    let mut count = 0;
    loop {
        let mut packet = [0; 128];
        //let rx = uart_rx.into_buffered(&mut packet);
        match uart_rx.read_packet(&mut packet).await {
            Ok(n) => {
                if n == 0 {
                    continue;
                }
                let message = &packet[0..n];
                info!("{count}: Got {n} bytes ({})", packet[0]);
                count += 1;
                //continue;

                if message[0] == b'm' {
                    info!(
                        "{}/{} bytes are allocated",
                        ALLOCATOR.capacity() - ALLOCATOR.free(),
                        ALLOCATOR.capacity()
                    );
                } else if message[0] == b'p' {
                    let start = Instant::now();
                    reset_profile();
                    let reset_time = (Instant::now() - start).as_ticks();
                    Timer::after(Duration::from_millis(1000)).await;
                    let mut profile = take_profile();
                    let total_ticks = profile.system.idle_ticks + profile.system.busy_ticks;
                    info!(
                        "{} tick profile. {:.2}% busy.",
                        total_ticks,
                        profile.system.busy_ticks as f32 / total_ticks as f32 * 100.0
                    );
                    if let Some(longest_exec) =
                        profile.tasks.iter().max_by_key(|t| t.longest_execution)
                    {
                        info!(
                            " - Longest exec time: Task {} for {} ticks",
                            longest_exec.name, longest_exec.longest_execution
                        );
                    }

                    profile
                        .tasks
                        .as_mut_slice()
                        .sort_unstable_by_key(|t| t.run_ticks);

                    for t in profile.tasks.iter().rev() {
                        info!(
                            " - Task {} took {} ticks (longest {} ticks)",
                            t.name, t.run_ticks, t.longest_execution
                        );
                        Timer::after(Duration::from_millis(100)).await;
                    }
                    info!("Reset time = {reset_time} ticks");

                    /*if let Some(highest_cpu) = profile.tasks.iter().max_by_key(|t| t.run_ticks) {
                        info!(
                            " - Busiest task: {} for {} ticks",
                            highest_cpu.name, highest_cpu.run_ticks
                        );
                        Timer::after(Duration::from_millis(100)).await;
                        if let Some(second_highest_cpu) = profile
                            .tasks
                            .iter()
                            .filter(|t| t.name != highest_cpu.name)
                            .max_by_key(|t| t.run_ticks)
                        {
                            info!(
                                " - Second busiest task: {} for {} ticks",
                                second_highest_cpu.name, second_highest_cpu.run_ticks
                            );
                        }
                    }*/
                } else if message[0] == b'i' {
                    let duration = { *I2C_DURATION.lock().await };
                    let value = { *I2C_FINAL_VALUE.lock().await };
                    info!("I2C loop duration: {duration} ticks value: {value:x}");
                } else if message[0] == b'a' {
                    for i in 0..8 {
                        let raw = {
                            let channels = ANALOG_CHANNELS.lock().await;
                            channels[i]
                        };
                        info!("Channel {i}: raw={raw} mapped={}", map_analog_channel(raw));
                        Timer::after(Duration::from_millis(50)).await;
                    }
                } else if message[0] == b'b' {
                    let value = { *I2C_FINAL_VALUE.lock().await };
                    info!("0x{value:x}");
                    if !value != 0 {
                        info!("- Lowest 0 bit is {}", (!value).lowest_one().unwrap_or(100));
                    }
                }
            }
            Err(e) => {
                info!("Error reading endpoint: {e:?}");
            }
        }
        //Timer::after(Duration::from_millis(10)).await;
    }
}

#[embassy_executor::task]
async fn usb_task(
    mut usb: embassy_usb::UsbDevice<'static, embassy_stm32::usb::Driver<'static, USB>>,
) {
    usb.run().await;
}

fn map_analog_channel(input: u16) -> i8 {
    ((input >> 4) as i16 - 128) as i8
}

#[embassy_executor::task]
async fn joystick_update_task(
    mut joy_writer: embassy_usb::class::hid::HidWriter<
        'static,
        embassy_stm32::usb::Driver<'static, USB>,
        16,
    >,
) {
    let mut report = JoyReport {
        axis0: 0,
        axis1: 0,
        axis2: 0,
        axis3: 0,
        axis4: 0,
        axis5: 0,
        axis6: 0,
        axis7: 0,
        buttons0: 0,
        buttons1: 0,
        buttons2: 0,
        buttons3: 0,
        buttons4: 0,
        buttons5: 0,
        buttons6: 0,
        buttons7: 0,
    };
    let mut ticker = Ticker::every(Duration::from_millis(50));
    let mut encoder_count = [0; 6];
    loop {
        //joy_writer.write(&[0]).await;
        /*report.buttons0 = 0xff;
        match joy_writer.write_serialize(&report).await {
            Ok(_) => {}
            Err(e) => {
                info!("foo");
            }
        }
        Timer::after(Duration::from_millis(500)).await;*/
        report.buttons0 = 0;

        // 0xffffffbf_ffff_ffff
        let buttons_in = { *I2C_FINAL_VALUE.lock().await };
        /*if buttons & (1 << 38) == 0 {
            report.buttons0 |= 1;
        }*/
        let mut buttons = 0u64;
        for (from, to) in DEVICE_MAPPING.buttons.iter().enumerate() {
            if buttons_in & (1 << *to) == 0 {
                buttons |= 1 << from;
            }
        }

        assert_eq!(DEVICE_MAPPING.buttons.len(), 16);
        for (to, &(up, down)) in DEVICE_MAPPING.dual_throws.iter().enumerate() {
            if buttons_in & (1 << up) == 0 {
                buttons |= 1 << (to * 2 + 16);
            }
            if buttons_in & (1 << down) == 0 {
                buttons |= 1 << (to * 2 + 17);
            }
        }

        {
            let mut pulses = ENCODER_PULSES.lock().await;
            for (to, &(press, a, b)) in DEVICE_MAPPING.encoders.iter().enumerate() {
                let base =
                    to * 3 + DEVICE_MAPPING.buttons.len() + 2 * DEVICE_MAPPING.dual_throws.len();
                if buttons_in & (1 << press) == 0 {
                    buttons |= 1 << base;
                }

                let pulse_active =
                    (buttons & (1 << (base + 1))) != 0 || (buttons & (1 << (base + 2))) != 0;

                if pulse_active {
                    buttons &= !(1 << (base + 1));
                    buttons &= !(1 << (base + 2));
                } else if let Some(pulse) = pulses[to].pop() {
                    match pulse {
                        EncoderDirection::Decrement => {
                            encoder_count[to] -= 1;
                        }
                        EncoderDirection::Increment => {
                            encoder_count[to] += 1;
                        }
                    }
                    info!("{to}: {pulse:?} -> {}", encoder_count[to]);
                    if pulse == EncoderDirection::Decrement {
                        buttons |= 1 << (base + 1);
                    } else {
                        assert_eq!(pulse, EncoderDirection::Increment);
                        buttons |= 1 << (base + 2);
                    }
                }
            }
        }

        report.buttons0 = (buttons & 0xff) as u8;
        report.buttons1 = ((buttons >> 8) & 0xff) as u8;
        report.buttons2 = ((buttons >> 16) & 0xff) as u8;
        report.buttons3 = ((buttons >> 24) & 0xff) as u8;
        report.buttons4 = ((buttons >> 32) & 0xff) as u8;
        report.buttons5 = ((buttons >> 40) & 0xff) as u8;
        report.buttons6 = ((buttons >> 48) & 0xff) as u8;
        report.buttons7 = ((buttons >> 56) & 0xff) as u8;

        {
            let channels = ANALOG_CHANNELS.lock().await;
            report.axis0 = map_analog_channel(channels[DEVICE_MAPPING.axes[0]]);
            report.axis1 = map_analog_channel(channels[DEVICE_MAPPING.axes[1]]);
            report.axis2 = map_analog_channel(channels[DEVICE_MAPPING.axes[2]]);
            report.axis3 = map_analog_channel(channels[DEVICE_MAPPING.axes[3]]);
            report.axis4 = map_analog_channel(channels[DEVICE_MAPPING.axes[4]]);
            report.axis5 = map_analog_channel(channels[DEVICE_MAPPING.axes[5]]);
            report.axis6 = map_analog_channel(channels[DEVICE_MAPPING.axes[6]]);
            report.axis7 = map_analog_channel(channels[DEVICE_MAPPING.axes[7]]);
        }

        if let Err(e) = joy_writer.write_serialize(&report).await {
            info!("Joystick update error: {e:?}");
        }
        //Timer::after(Duration::from_millis(5)).await;
        ticker.next().await;
    }
}

#[embassy_executor::task]
async fn joystick_input_task(
    joy_reader: embassy_usb::class::hid::HidReader<
        'static,
        embassy_stm32::usb::Driver<'static, USB>,
        1,
    >,
    mut request_handler: JoyRequestHandler,
) {
    joy_reader.run(false, &mut request_handler).await;
}

static ANALOG_CHANNELS: Mutex<ThreadModeRawMutex, [u16; 8]> = Mutex::new([0; 8]);

const ADC_FILTER_LENGTH: usize = 4;

struct FirFilter {
    samples: [u16; ADC_FILTER_LENGTH],
    head: usize,
}

impl FirFilter {
    const fn new() -> Self {
        Self {
            samples: [0; ADC_FILTER_LENGTH],
            head: 0,
        }
    }

    fn push(&mut self, sample: u16) -> u16 {
        self.samples[self.head] = sample;
        self.head = (self.head + 1) % self.samples.len();
        (self.samples.iter().map(|x| *x as u32).sum::<u32>() / self.samples.len() as u32) as u16
    }
}

#[embassy_executor::task]
async fn adc_task(
    mut adc: Adc<'static, ADC1>,
    mut channels: [AnyAdcChannel<'static, ADC1>; 8],
    mut reference: AnyAdcChannel<'static, ADC1>,
) {
    let mut filters = [const { FirFilter::new() }; 8];
    let mut ticker = Ticker::every(Duration::from_millis(5));
    loop {
        let mut results = [0; 8];
        for i in 0..8 {
            results[i] = filters[i].push(
                adc.read(&mut channels[i], adc::SampleTime::CYCLES239_5)
                    .await,
            );

            // Read the reference voltage to reset any charge in case the next channel is open
            let _ = adc.read(&mut reference, adc::SampleTime::CYCLES239_5).await;
        }
        {
            let mut channels = ANALOG_CHANNELS.lock().await;
            for i in 0..8 {
                channels[i] = results[i];
            }
        }
        ticker.next().await;
    }
}

enum I2cTask {
    Poll(usize),
    Assemble,
}

impl I2cTask {
    fn next(&self) -> Self {
        match self {
            Self::Poll(n) if *n >= 3 => Self::Assemble,
            Self::Poll(n) => Self::Poll(*n + 1),
            Self::Assemble => Self::Poll(0),
        }
    }
}

const I2C_ADDRESSES: [u8; 4] = [0x20, 0x22, 0x24, 0x26];

static I2C_DURATION: Mutex<ThreadModeRawMutex, u64> = Mutex::new(0);
static I2C_LAST_ASSEMBLE_TIME: Mutex<ThreadModeRawMutex, Instant> =
    Mutex::new(Instant::from_millis(0));
static I2C_VALUE: Mutex<ThreadModeRawMutex, u64> = Mutex::new(0xffff_ffff_ffff_ffff);
static I2C_FINAL_VALUE: Mutex<ThreadModeRawMutex, u64> = Mutex::new(0xffff_ffff_ffff_ffff);
static I2C_TASK_CHANNEL: Channel<ThreadModeRawMutex, I2cTask, 8> = Channel::new();
static ENCODERS: Mutex<ThreadModeRawMutex, [EncoderState; 6]> =
    Mutex::new([const { EncoderState::new() }; 6]);
static ENCODER_PULSES: Mutex<ThreadModeRawMutex, [PulseState; 6]> =
    Mutex::new([const { PulseState::new() }; 6]);

#[embassy_executor::task]
async fn interrupt_i2c_task(
    mut i2c: I2cDevice<'static, ThreadModeRawMutex, I2c<'static, Async, Master>>,
    interrupts: [GpioInput<'static>; 4],
) {
    let mut encoder_pin_state = [(false, false); 6];
    let mut encoder_count = [0; 6];

    loop {
        for (index, input) in interrupts.iter().enumerate() {
            if input.is_low() {
                //info!("{index}");

                // Poll the corresponding I2C device
                let mut result = [0, 0];
                if let Err(e) = i2c
                    .write_read(I2C_ADDRESSES[index], &[0x0], &mut result)
                    .await
                {
                    //
                }

                let value = {
                    let mut value = I2C_VALUE.lock().await;
                    *value &= !(0xffff << (index * 16));
                    *value |= (result[0] as u64) << (index * 16);
                    *value |= (result[1] as u64) << (index * 16 + 8);
                    *value
                };

                {
                    let mut final_value = I2C_FINAL_VALUE.lock().await;
                    *final_value = value;
                }

                {
                    let mut encoders = ENCODERS.lock().await;
                    let mut pulses = ENCODER_PULSES.lock().await;
                    for (i, ((encoder, pulses), &(_, a, b))) in encoders
                        .iter_mut()
                        .zip(pulses.iter_mut())
                        .zip(DEVICE_MAPPING.encoders.iter())
                        .enumerate()
                    {
                        let a = value & (1 << a) == 0;
                        let b = value & (1 << b) == 0;
                        if (a, b) != encoder_pin_state[i] {
                            info!("{i}: {a} {b}");
                            encoder_pin_state[i] = (a, b);
                        }
                        let pulse = encoder.update(a, b);
                        if let Some(pulse) = pulse {
                            pulses.push(pulse);
                            match pulse {
                                EncoderDirection::Decrement => {
                                    encoder_count[i] -= 1;
                                }
                                EncoderDirection::Increment => {
                                    encoder_count[i] += 1;
                                }
                            }
                            info!("{i}: {pulse:?} -> {}", encoder_count[i]);
                        }
                    }
                }
            }
        }
        Timer::after(Duration::from_micros(50)).await;
    }
}

#[embassy_executor::task(pool_size = 2)]
async fn i2c_task(mut i2c: I2cDevice<'static, ThreadModeRawMutex, I2c<'static, Async, Master>>) {
    let mut encoder_pin_state = [(false, false); 6];
    let mut encoder_count = [0; 6];
    loop {
        let task = I2C_TASK_CHANNEL.receive().await;

        match task {
            I2cTask::Poll(index) => {
                // Tee up the next operation
                I2C_TASK_CHANNEL.try_send(task.next());

                // Poll
                let mut result = [0, 0];
                if let Err(e) = i2c
                    .write_read(I2C_ADDRESSES[index], &[0x0], &mut result)
                    .await
                {
                    //
                }

                let mut value = I2C_VALUE.lock().await;
                *value &= !(0xffff << (index * 16));
                *value |= (result[0] as u64) << (index * 16);
                *value |= (result[1] as u64) << (index * 16 + 8);
            }
            I2cTask::Assemble => {
                let value = { *I2C_VALUE.lock().await };

                // Tee up the next operation
                I2C_TASK_CHANNEL.try_send(task.next());

                {
                    let mut final_value = I2C_FINAL_VALUE.lock().await;
                    /*if *final_value != value {
                        info!("{value:x}");
                    }*/
                    *final_value = value;
                }

                {
                    let mut encoders = ENCODERS.lock().await;
                    let mut pulses = ENCODER_PULSES.lock().await;
                    for (i, ((encoder, pulses), &(_, a, b))) in encoders
                        .iter_mut()
                        .zip(pulses.iter_mut())
                        .zip(DEVICE_MAPPING.encoders.iter())
                        .enumerate()
                    {
                        let a = value & (1 << a) == 0;
                        let b = value & (1 << b) == 0;
                        if (a, b) != encoder_pin_state[i] {
                            //info!("{i}: {a} {b}");
                            encoder_pin_state[i] = (a, b);
                        }
                        let pulse = encoder.update(a, b);
                        if let Some(pulse) = pulse {
                            pulses.push(pulse);
                            match pulse {
                                EncoderDirection::Decrement => {
                                    encoder_count[i] -= 1;
                                }
                                EncoderDirection::Increment => {
                                    encoder_count[i] += 1;
                                }
                            }
                            //info!("{i}: {pulse:?} -> {}", encoder_count[i]);
                        }
                    }
                }

                let duration = {
                    let mut last_time = I2C_LAST_ASSEMBLE_TIME.lock().await;
                    let now = Instant::now();
                    let duration = (now - *last_time).as_ticks();
                    *last_time = now;
                    duration
                };

                {
                    *I2C_DURATION.lock().await = duration;
                }

                //info!("{value:x} {duration}");
            }
        }
    }

    /*loop {
        let mut value = 0;
        let start = Instant::now();
        for (i, address) in [0x20, 0x22, 0x24, 0x26].iter().enumerate() {
            let mut result = [0, 0];
            if let Err(e) = i2c.write_read(*address, &[0x0], &mut result).await {
                //
            }

            value |= (result[0] as u64) << (i * 16);
            value |= (result[1] as u64) << (i * 16 + 8);
        }

        info!("{:x} in {} ticks", value, start.elapsed().as_ticks());
        //info!("a={} b={}", a[0], b[0]);

        Timer::after(Duration::from_millis(5000)).await;
    }*/
}

/// Linux can handle it, but Windows cannot
const ENABLE_SERIAL: bool = false;

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    set_panic_count(1);

    let mut config = embassy_stm32::Config::default();
    {
        use embassy_stm32::rcc::*;

        config.rcc.hsi48 = Some(Hsi48Config {
            sync_from_usb: true,
        });
        config.rcc.sys = Sysclk::HSI48;
        config.rcc.ahb_pre = AHBPrescaler::DIV1;
        config.rcc.apb1_pre = APBPrescaler::DIV1;
    }

    let p = embassy_stm32::init(config);
    let mut led = Output::new(p.PB8, Level::Low, Speed::Low);

    let mut int0 = GpioInput::new(p.PB3, embassy_stm32::gpio::Pull::Up);
    let mut int1 = GpioInput::new(p.PB4, embassy_stm32::gpio::Pull::Up);
    let mut int2 = GpioInput::new(p.PB5, embassy_stm32::gpio::Pull::Up);
    let mut int3 = GpioInput::new(p.PB12, embassy_stm32::gpio::Pull::Up);

    let mut adc = Adc::new(p.ADC1, Irqs);
    let vref = adc.enable_vref();

    let mut i2c_config = embassy_stm32::i2c::Config::default(); /*{
    frequency: Hertz::khz(400),
    gpio_speed: Speed::VeryHigh,
    sda_pullup: true,
    scl_pullup: true,
    timeout: Duration::from_millis(500),
    };*/
    i2c_config.frequency = Hertz::khz(400);
    i2c_config.timeout = Duration::from_millis(50);
    let mut i2c = I2c::new(
        p.I2C2, p.PB10, p.PB11, p.DMA1_CH4, p.DMA1_CH5, Irqs, i2c_config,
    );

    let driver = Driver::new(p.USB, Irqs, p.PA12, p.PA11);

    // Note: This VID/PID is a reserved testing PID
    let mut config = embassy_usb::Config::new(0x1209, 0x000d);
    config.manufacturer = Some("Pillow Computing Consortium");
    config.product = Some("Flightpanel");
    config.serial_number = Some(embassy_stm32::uid::uid_hex());
    config.composite_with_iads = ENABLE_SERIAL;
    if !ENABLE_SERIAL {
        config.device_class = 3; // USB HID
        config.device_sub_class = 0; // No subclass (i.e. not boot)
        config.device_protocol = 0; // No protocol (i.e. neither mouse nor keyboard)
    }

    // Create embassy-usb DeviceBuilder using the driver and config.
    // It needs some buffers for building the descriptors.
    let mut config_descriptor = ALLOCATOR.alloc([0; 256]);
    let mut bos_descriptor = ALLOCATOR.alloc([0; 256]);
    let mut control_buf = ALLOCATOR.alloc([0; 64]);

    let mut state = ALLOCATOR.alloc(State::new());

    let mut hid_state = ALLOCATOR.alloc(hid::State::new());
    let mut hid_report_descriptor = [0; 512];

    let mut builder = Builder::new(
        driver,
        config,
        config_descriptor,
        bos_descriptor,
        &mut [], // no msos descriptors
        control_buf,
    );

    // Create classes on the builder.
    let mut class = if ENABLE_SERIAL {
        Some(CdcAcmClass::new(&mut builder, state, 64))
    } else {
        None
    };

    //let hid_report_descriptor_size = build_report_descriptor(&mut hid_report_descriptor);
    let mut hid_class = hid::Config {
        report_descriptor: JoyReport::desc(),
        request_handler: None,
        poll_ms: 10,
        max_packet_size: 64,
        hid_boot_protocol: hid::HidBootProtocol::None,
        hid_subclass: hid::HidSubclass::No,
    };
    let rw = hid::HidReaderWriter::<_, 1, 16>::new(&mut builder, hid_state, hid_class);

    let mut request_handler = JoyRequestHandler {};

    // Build the builder.
    let mut usb = builder.build();

    // Run the USB device.
    //let usb_fut = usb.run();

    let (joy_reader, mut joy_writer) = rw.split();

    /*let mut uart_buf = [
        PendingMessage::new(),
        PendingMessage::new(),
        PendingMessage::new(),
    ];*/
    //let mut uart_channel = Channel::<ThreadModeRawMutex, _, _>::new();
    //let (mut log_tx, mut log_rx) = uart_channel.split();
    //UART_SENDER.init(uart_channel.sender());
    //let log_rx = UART_CHANNEL.receiver();
    //let log_rx = log_receiver();

    //UART_SENDER.init(uart_tx);

    // Do stuff with the class!
    /*let echo_fut = async {
        //let (tx, rx) = class.split();

        loop {
            class.wait_connection().await;
            info!("Connected");
            let _ = echo(&mut class).await;
            info!("Disconnected");
        }
    };*/

    /*let uart_send_fut = async {
        uart_tx.wait_connection().await;
        while let message = log_rx.receive().await {
            uart_tx.wait_connection().await;
            for i in 0..message.len {
                uart_tx.write_packet(&[message.message[i]]).await;
            }
            //log_rx.receive_done();
        }
    };*/

    I2C_TASK_CHANNEL.send(I2cTask::Poll(0)).await;

    if let Some(class) = class {
        let (mut uart_tx, mut uart_rx) = class.split();

        let task = crate::log::log_task(uart_tx).unwrap();
        set_task_name(task.id(), "log");
        spawner.spawn(task);

        let task = cli_task(uart_rx).unwrap();
        set_task_name(task.id(), "cli");
        spawner.spawn(task);
    }

    let task = led_task(led).unwrap();
    set_task_name(task.id(), "led");
    spawner.spawn(task);

    let task = usb_task(usb).unwrap();
    set_task_name(task.id(), "usb");
    spawner.spawn(task);

    let task = joystick_update_task(joy_writer).unwrap();
    set_task_name(task.id(), "joystick_update");
    spawner.spawn(task);

    let task = joystick_input_task(joy_reader, request_handler).unwrap();
    set_task_name(task.id(), "joystick_input");
    spawner.spawn(task);

    let task = adc_task(
        adc,
        [
            p.PA0.degrade_adc(),
            p.PA1.degrade_adc(),
            p.PA2.degrade_adc(),
            p.PA3.degrade_adc(),
            p.PA4.degrade_adc(),
            p.PA5.degrade_adc(),
            p.PA6.degrade_adc(),
            p.PA7.degrade_adc(),
        ],
        vref.degrade_adc(),
    )
    .unwrap();
    set_task_name(task.id(), "adc");
    spawner.spawn(task);

    set_panic_count(2);

    //static I2C_BUS: StaticCell<Mutex<NoopRawMutex, _>> = StaticCell::new();
    let i2c_bus = ALLOCATOR.alloc(Mutex::new(i2c));
    //let i2c_bus = Mutex::new(i2c);
    //let i2c_bus = I2C_BUS.init(i2c_bus);
    let i2c1 = I2cDevice::new(i2c_bus);
    //let i2c2 = I2cDevice::new(i2c_bus);

    //set_panic_count(3);

    let task = interrupt_i2c_task(i2c1, [int2, int0, int3, int1]).unwrap();
    set_task_name(task.id(), "i2c");
    spawner.spawn(task);

    /*let task = i2c_task(i2c1).unwrap();
    set_task_name(task.id(), "i2c1");
    spawner.spawn(task);

    set_panic_count(4);

    let task = i2c_task(i2c2).unwrap();
    set_task_name(task.id(), "i2c2");
    spawner.spawn(task);*/

    /*let uart_recv_fut = async {
        loop {
            uart_rx.wait_connection().await;
            let mut packet = [0; 128];
            uart_rx.read_packet(&mut packet).await;
        }
    };*/

    /*let led_fut = {
        //let log_tx = log_tx.clone();
        async move {
            loop {
                info!("Hello, World!");
                led.set_high();
                Timer::after(Duration::from_millis(500)).await;
                led.set_low();
                Timer::after(Duration::from_millis(500)).await;
            }
        }
    };*/

    /*let joy_fut = async {
        loop {
            //joy_writer.write(&[0]).await;
            match joy_writer.write_serialize(&JoyReport { x: 1, y: 0 }).await {
                Ok(_) => {}
                Err(e) => {
                    info!("foo");
                }
            }
            Timer::after(Duration::from_millis(500)).await;
            joy_writer.write_serialize(&JoyReport { x: 0, y: 1 }).await;
            Timer::after(Duration::from_millis(500)).await;
        }
    };

    let joy_out_fut = async {
        joy_reader.run(false, &mut request_handler).await;
    };*/

    // Run everything concurrently.
    // If we had made everything `'static` above instead, we could do this using separate tasks instead.
    //join2(joy_fut, joy_out_fut).await;

    // All the tasks are set up, so just hang out
    /*loop {
        Timer::after(Duration::from_millis(1000)).await;
    }*/

    /*loop {
        info!("Hello, World!");
        led.set_high();
        Timer::after(Duration::from_millis(500)).await;
        led.set_low();
        Timer::after(Duration::from_millis(500)).await;
    }*/

    set_panic_count(5);
}
