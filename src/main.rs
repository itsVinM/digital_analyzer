#![no_std]
#![no_main]

use panic_halt as _;
use stm32f4xx_hal as hal;
use rtic::app;
use core::time::Duration;
use hal::prelude::*; // Import traits like .split() and .constrain()
use hal::serial::config::Config;
use fugit::RateExtU32;

// Telemetry Structures for Modular Shared State
use fugit::ExtU32;

/// Holds all polled and validated data points for the entire system.
pub struct TelemetryData {
    pub voltage_rms: f32,
    pub current_rms: f32,
    pub validation_status: ValidationStatus, 
}

#[derive(Debug, PartialEq, Copy, Clone)]
pub enum ValidationStatus {
    Initializing,
    Polling,
    TestInProgress,
    Pass,
    Fail,
}

impl Default for TelemetryData {
    fn default() -> Self {
        TelemetryData {
            voltage_rms: 0.0,
            current_rms: 0.0,
            validation_status: ValidationStatus::Initializing,
        }
    }
}

// Constants (using fugit for type-safe duration)
const POLLING_INTERVAL: u32 = 100;
const CONTROL_SEQUENCE_START: u32 = 5000;
const HEARTBEAT_INTERVAL: u32 = 500;

// Setup the RTIC Monotonic Timer using TIM2.
// NOTE: You need to add `rtic-monotonics` crate and enable the `stm32` feature.
use rtic_monotonics::systick::*; // Systick is simpler for the F401RE

#[app(device = hal::pac, peripherals = true, dispatchers = [USART1])]
mod app {
    use super::*;
    use hal::gpio::*;
    use hal::pac;
    use core::fmt::Write; // For debug logging via serial

    // ----------------------------------------------------------------------
    // SHARED RESOURCES (The modular approach)
    // ----------------------------------------------------------------------
    #[shared]
    struct Shared {
        product_telemetry: TelemetryData,
    }

    // ----------------------------------------------------------------------
    // LOCAL RESOURCES (Peripherals)
    // ----------------------------------------------------------------------
    #[local]
    struct Local {
        // Nucleo LED is PC13, not PA5 on the F401RE
        led: PC13<Output<PushPull>>, 
        // We now hold the Uart peripherals
        product_uart_tx: hal::serial::Tx<pac::USART1>,
        product_uart_rx: hal::serial::Rx<pac::USART1>,
        debug_uart: hal::serial::Tx<pac::USART2>, // For logging to PC
    }

    // ----------------------------------------------------------------------
    // INITIALIZATION
    // ----------------------------------------------------------------------
    #[init]
    fn init(mut cx: init::Context) -> (Shared, Local, init::Monotonics) {
        // Promote HAL peripherals
        let dp: hal::pac::Peripherals = cx.device;
        let c_peripherals = cx.core;

        // 1. Clock Configuration (using HSI as default)
        let rcc = dp.RCC.constrain();
        let clocks = rcc.cfgr.use_hse(8.mhz()).sysclk(84.mhz()).freeze(); // 8Mhz HSE, 84Mhz Sysclk

        // 2. Monotonic Timer Setup
        // Initialize Systick as the RTIC monotonic timer
        // We use the Monotonic trait implementation from rtic-monotonics
        Systick::start(c_peripherals.SYST, clocks.sysclk().to_Hz());
        
        // 3. GPIO Setup
        let gpioa = dp.GPIOA.split();
        let gpioc = dp.GPIOC.split();
        
        // LED: PC13 (User LED on Nucleo F401RE)
        let mut led = gpioc.pc13.into_push_pull_output();
        led.set_low().unwrap(); // Turn off LED initially

        // 4. USART2 (Debug Link) - PA2 (TX), PA3 (RX) - AF7
        let tx2 = gpioa.pa2.into_alternate_af7();
        let rx2 = gpioa.pa3.into_alternate_af7();
        let (mut debug_uart, _) = hal::serial::Serial::usart2(
            dp.USART2, (tx2, rx2), 
            Config::default().baudrate(115200.bps()), // Standard debug rate
            &clocks
        ).unwrap().split();

        // 5. USART1 (Product Link) - PA9 (TX), PA10 (RX) - AF7
        let tx1 = gpioa.pa9.into_alternate_af7();
        let rx1 = gpioa.pa10.into_alternate_af7();
        let (product_uart_tx, product_uart_rx) = hal::serial::Serial::usart1(
            dp.USART1, (tx1, rx1), 
            Config::default().baudrate(9600.bps()), // Check your product manual!
            &clocks
        ).unwrap().split();

        // Log boot message (optional)
        writeln!(debug_uart, "System Booted. Starting tasks.").unwrap();


        // 6. TASK SCHEDULING
        // Note the use of .millis() from fugit
        polling_task::spawn().unwrap();
        heartbeat_task::spawn().unwrap();
        control_task::spawn_after(CONTROL_SEQUENCE_START.millis()).unwrap();
        
        (
            Shared { product_telemetry: TelemetryData::default() },
            Local { led, product_uart_tx, product_uart_rx, debug_uart },
            init::Monotonics(), // Initialize the Systick Monotonic
        )
    }

    // ----------------------------------------------------------------------
    // TASKS
    // ----------------------------------------------------------------------

    // --- Task 1: Heartbeat (Low Priority) ---
    #[task(local = [led])]
    fn heartbeat_task(cx: heartbeat_task::Context) {
        cx.local.led.toggle().unwrap();
        heartbeat_task::spawn_after(HEARTBEAT_INTERVAL.millis()).unwrap();
    }

    // --- Task 2: Constant Polling (Medium Priority) ---
    #[task(local = [product_uart_tx, product_uart_rx, debug_uart], shared = [product_telemetry])]
    fn polling_task(mut cx: pooling_task::Context) {
        let (tx, rx) = (cx.local.product_uart_tx, cx.local.product_uart_rx);
        let debug_tx = cx.local.debug_uart;
        
        // 1. Send GET Command for Voltage
        let cmd = create_get_voltage_cmd(); 
        
        // --- This is where non-blocking I/O is crucial ---
        // For simplicity here, we use a *mock* exchange:
        let result = tx_rx_protocol_exchange(tx, rx, &cmd);
        
        if let Ok(voltage) = result {
             // 2. Update Shared Resource Safely
             cx.shared.product_telemetry.lock(|data| {
                data.voltage_rms = voltage;
                data.validation_status = ValidationStatus::Polling; // Update system status
             });
             // Log success to PC debug terminal
             let _ = writeln!(debug_tx, "Polling: V={:.2}V", voltage);
        } else {
             let _ = writeln!(debug_tx, "Polling failed.");
        }

        // 3. Schedule next execution
        polling_task::spawn_after(POLLING_INTERVAL.millis()).unwrap();
    }

    // --- Task 3: Control and Validation Sequence (High Priority) ---
    #[task(local = [product_uart_tx, product_uart_rx, debug_uart], shared =[product_telemetry])]
    fn control_task(mut cx: control_task::Context) {
        let (tx, rx) = (cx.local.product_uart_tx, cx.local.product_uart_rx);
        let debug_tx = cx.local.debug_uart;

        cx.shared.product_telemetry.lock(|data| data.validation_status = ValidationStatus::TestInProgress);
        let _ = writeln!(debug_tx, "--- Starting Verification Sequence ---");

        // --- Step 1: Set a Control Limit ---
        let set_cmd = create_set_limit_cmd(10.0);
        
        if tx_rx_protocol_exchange(tx, rx, &set_cmd).is_err() {
            cx.shared.product_telemetry.lock(|data| data.validation_status = ValidationStatus::Fail);
            let _ = writeln!(debug_tx, "Verification FAIL: Could not set limit.");
            return;
        }
        let _ = writeln!(debug_tx, "Limit set successfully.");

        // --- Step 2: Use Polling Data for Context ---
        let current_v = cx.shared.product_telemetry.lock(|data| data.voltage_rms);
        let _ = writeln!(debug_tx, "Current Polled Voltage: {:.2}V", current_v);
        
        // We do not re-spawn this task unless we want it to loop/retry.
        // For a one-shot validation, the task simply completes.
    }

    // ----------------------------------------------------------------------
    // HELPER FUNCTIONS (Placeholders for Protocol/IO Logic)
    // ----------------------------------------------------------------------

    /// Placeholder: In a real app, this MUST be non-blocking (Interrupts/DMA)
    fn tx_rx_protocol_exchange(tx: &mut hal::serial::Tx<pac::USART1>, rx: &mut hal::serial::Rx<pac::USART1>, cmd: &[u8]) -> Result<f32, ()> {
        // Send command bytes, wait for response, parse, and validate CRC
        
        // Mock success return
        Ok(10.5) 
    }

    /// Logs message to the debug UART (blocking for simplicity here)
    fn log_debug_message(tx: &mut hal::serial::Tx<pac::USART2>, message: &str) {
         // The writeln! macro handles the conversion and output, but it's blocking in this context.
         let _ = writeln!(tx, "{}", message);
    }


    fn create_get_voltage() -> [u8: 8]{ [0x01, 0x03, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00] }
    fn create_get_limit_cmd() -> [u8; 8] { [0x01, 0x03, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00] }
    fn create_set_limit_cmd(_limit: f32) -> [u8:8]{ [0x01, 0x03, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00] }
    fn setup_uart1_pins(uart: &pac::USART1, ...) -> (hal::serial::Tx<pac::USART1>, hal::serial::Rx<pac::USART1>){todo!()}
    fn setup_usart2_debug_pins(uart: &pac::USART2, ...) -> hal::serial::Tx<pac::USART2> {todo!()}


}