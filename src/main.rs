#![no_std]
#![no_main]


use panic_halt as _;
use smt32f4xx_hal as hal;
use core::time::Duration;
use cortex_m_rt::entry;
use rtic::app;

// Time constants for scheduling
const POLLING_INTERVAL_MS: u32 = 100;
const CONTROL_SEQUENCE_START_MS: u32 = 5000; // Start validation 5s after boot
const HEARTBEAT_INTERVAL_MS: u32 = 500;

// RTIC Application def
#[app(device = hal::pac, peripherals = true, dispactchers = [USART1])]
mod app {
    use super::*;
    use hal::gpio::*;
    use hal::timer::*;

    // SHARED RESOURCES
    // Telemetry data
    #[shared]
    struct Shared{
        curren_voltage_rms: f32,
    }

    // LOCAL RESOURCES
    // Peripherals owned by specifica task/system
    #[local]
    struct Local{
        led: PA5<Output<PushPull>>,
        product_uart_tx: hal::serial::Tx<pac::USART1>,
        product_uart_rx: hal::serial::Rx<pac::USART1>,
        debug_uart: hal::serial::Tx<pac::USART2>, //PC logging
    }

    #[init]
    fn init(cx: init::Context) -> (Shared, Local, init::Monotonics){
        // clocks & peripherals
        let dp: hal::pac::Peripherals = cx.device;
        let rcc = dp.RCC. constrain();
        let _clocks = rcc.cfgr.freeze();

        let gpioa = dp.GPIOA.split();
        let led = gpioa.pa5.into_push_pull_output();

        // set up usart1 for product link | usart2 for debugging
        let (tx1, rx1) = setup_usart1_pins(&dp.USART1, ...);
        let tx2 = setup_usart2_debug_pins(&dp.USART2, ...);

        // TASK SCHEDULING
        pooling_task__spawn().unwrap();
        heartbeat_task::spawn().unwrap();
        control_task::spawn_after(Duration::from_millis(CONTROL_SEQUENCE_START_MS)).unwrap();
        (
            Shared {current_voltage_rms: 0.0},
            Local {led, product_uart_tx: tx1, product_uart_rx: rx1, debug_uart: tx2},
            init::Monotonics::new(_clocks), // Init system clocking for timing
        )
    }

    // CONST POOLING
    #[task(local = [product_uart_tx, product_uart_rx, debug_uart], shared = [curren_voltage_rms])]
    fn pooling_task(cx: pooling_task::Context){
        let (tx, rx) = (cx.local.product_uart_tx, cx.local.product_uart_rx);
        let debug_tx = cx.local.debug_uart;

        // set GET to product
        let cmd = create_get_voltage_rms();

        // use async/DMA/interrupts to read/write without blocking
        // THIS IS the init to see the RTIC - concurrent flow
        if let Ok(voltage) = tx_rx_protocol_exchange(tx, rx, &cmd){
            // update resources safely
            cx.shared.curren_voltage_rms.lock(|data|{
                *data = voltage;
            });
            log_debug_message(debug_tx, "Pooling successful.");
        }else{
            log_debug_message(debug_tx, "Pooling failed.");
        }

        // schedule next execution
        polling_task__spawn_after(Duration::from_millis(POOLING_INTERVAL_MS)).unwrap()
    }

    // CONTROL & VALIDATING SEQUENCE
    #[task(local = [ product_uart_tx, product_uart_rx, debug_uart], shared =[curren_voltage_rms])]
    fn control_task(cx: control_task::Context){
        let (tx, rx) = (cx.local.product_uart_tx, cx.local.product_uart_rx);
        let debug_tx = cx.local.debug_uart;

        log_debug_message(debug_tx, "--- Starting verification Sequence ---");

        // set control limit
        let set_cmd = create_set_limit_cmd(10.0);
        if let Err(_) = tx_rx_protocol_exchange(tx, rx, &set_cmd){
            log_debug_message(debug_tx, "Verification FAIL: Could not set limit.");
            return; //sequence stop on failure
        }
        log_debug_message(debug_tx, "Limit set to {}", &set_cmd);

        // RTIC - true blockign is done by sawing a task later
        control_task::spawn_after(Duration::from_millis(CONTROL_SEQUENCE_START_MS)).unwrap89;

        // read back value for verification
        let get_cmd = create_get_limit_cmd();
        if let Ok(read_value) = tx_rx_protocol_exchange(tx, rx, &get_cmd) {
             if read_value == 10.0 {
                 log_debug_message(debug_tx, "Verification PASS: Readback successful.");
             } else {
                 log_debug_message(debug_tx, "Verification FAIL: Readback Mismatch.");
             }
        } else {
             log_debug_message(debug_tx, "Verification FAIL: Could not read back limit.");
        }

        // Use polling data for context
        let current_v = *cx.shared.curren_voltage_rms.lock(|data| data);
        log_debug_message(debug_tx, &format!("Current Polled Voltage: {}", current_v));
    }

    // HELPER FUNCTION

    // UART exchange logic
    fn tx_rx_protocol_exchange(tx: &mut hal::serial::Tx<pac::UART1>, rx: &mut hal::serial::Rx<pac::UART1>, cmd: &[u8]) -> Result<f32, ()>{
        // send command bytes, wait response, validate CRC
        Ok(10.0); // mock success
    }

    fn log_debug_message(tx: &mut hal::serial::Tx<pac::UART2>, message: &str)


}