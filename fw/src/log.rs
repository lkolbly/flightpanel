use core::{
    cell::SyncUnsafeCell,
    fmt::Write,
    ops::{Deref, DerefMut},
};

use embassy_stm32::peripherals::USB;
use embassy_sync::{
    blocking_mutex::raw::{RawMutex, ThreadModeRawMutex},
    channel::{Channel, Receiver},
};

struct PoolAllocator<M, T, const N: usize> {
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

            crate::log::log_args(args).await;

            /*if let Some(mut message) = crate::log::UART_MESSAGE_POOL.allocate() {
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
            }*/

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
}

use embassy_usb::{class::cdc_acm::Sender, driver::Driver};
pub(crate) use info;

pub async fn log_args<'a>(args: core::fmt::Arguments<'a>) {
    if let Some(mut message) = crate::log::UART_MESSAGE_POOL.allocate() {
        message.reset();
        core::fmt::write(&mut *message, args);
        message.write_str("\r\n");

        UART_CHANNEL.sender().send(message).await;
    }
}

pub fn log_receiver() -> Receiver<
    'static,
    ThreadModeRawMutex,
    PoolAllocatorGuard<'static, ThreadModeRawMutex, PendingMessage, 3>,
    3,
> {
    UART_CHANNEL.receiver()
}

#[embassy_executor::task]
pub async fn log_task(mut uart_tx: Sender<'static, embassy_stm32::usb::Driver<'static, USB>>) {
    let log_rx = log_receiver();

    uart_tx.wait_connection().await;
    while let message = log_rx.receive().await {
        uart_tx.wait_connection().await;
        for i in 0..message.len {
            uart_tx.write_packet(&[message.message[i]]).await;
        }
        //log_rx.receive_done();
    }
}
