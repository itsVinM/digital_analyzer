#![no_std]
#![no_main]

use panic_halt as _;
use stm32f4xx_hal as hal;
use rtic::app;

use hal::prelude::*;
use hal::serial::config::Config;
use fugit::RateExtU32;
use core::fmt::Write;
use heapless::{spsc::Queue, String, Vec}:
// Monotonic using systick
use rtic_monotonics::systick::Systick;

// Poll cadence and timeouts
const POLLING_INTERVAL: u32 = 1000;             // Pool every 1 s
const HEARTBEAT_INTERVAL: u32 = 500;            // Blink LED
const RESP_TIMEOUT_MS: u32 = 200;               // response timepout per request
const MAX_LINE_LEN: usize = 96;                 // expected max ASCII response

/// Telemetry container (portable; populate from parsed frames)
#[derive(Debug, Clone)]
pub struct TelemetryData {
    // AC input
    pub ac_voltage_rms: f32,    // VAC
    pub ac_frequency_hz: f32,   // Hz
    // DC output
    pub dc_output_voltage: f32, // V
    pub dc_output_current: f32, // A
    pub unit_temp: f32, // C

    // Battery & charger telemetry
    pub battery_voltage: f32,   // V
    pub battery_current: f32,   // A (charging + / discharging - if signed)

}

impl Default for TelemetryData {
    fn default() -> Self {
        TelemetryData {
            ac_voltage_rms:     0.0,
            ac_frequency_hz:    0.0,
            dc_output_voltage:  0.0,
            dc_output_current:  0.0,
            unit_temp:          0.0,
            battery_voltage:    0.0,
            battery_current:    0.0,
        }
    }
}


// RS232 request sent
#[derive(Debug, Clone, Copy)]
enum Request {
    ReadVout,
    ReadIout, 
    ReadBattV,
    ReadBattI,
    ReadTemps
    ReadAc, // voltage, frequency
    ReadState,
}

fn encode_request(req:Request, out: &mu Vec<u8, 64>){
    out.clear();
    // ASCII for EDS-500
    let cmd = match req {
        Request::ReadVout  => b"get v_out\r\n",
        Request::ReadIout  => b"get i_converter\r\n",
        Request::ReadBattV => b"get v_bat\r\n",
        Request::ReadBattI => b"gte i_bat\r\n",
        Request::ReadTemps => b"get temp_int\r\n",
        Request::States    => b"get state\r\n",
    };

    let _ = pout.extend_from_slice(cmd);
}

#[app(device = hal::pac, peripherals = true, dispatchers = [USART1])]
mod app {
    use super::*;
    use hal::gpio::*;
    use hal::pac;
    
    // --- Shared state ---
    #[share]
    struct Shared{
        telemetry: TelemetryData,
        req_index: u8, //scheduler state
    }

    // --- Local resources ---
    #[local]
    struct Local{
        // LED
        led::gpio::PA5<Output<PushPull>>,

        // Product UART
        product_tx: hal::serial::TX<pac::USART1>,
        product_rx: hal::serial::RX<pac::USART1>,

        // Debug UART
        debug_tx:   hal::serial::TX<pac::USART2>,

        // TX/RX buffers
        tx_buf: Vec<u8, 64>,
        rx_line: String<MAX_LINE_LEN>,
        // Response time
        awaiting_resp: bool,
    }

    // --- INIT --
    #[init]
    fn init(cx: init::Context) -> (Shared, Local, init::Monotonics){
        let dp = cx.device;
        let mut syst = cx.core.SYST;

        //clocks - hse 8MHz -> syscl 84 MHz
        let rcc = dp.RCC.constrain();
        let clock = rc..cfgr.use_hse(8.MHz()).sysclk(84.mhz()).freeze();

        // Start systick monotonic
        Systick::start(&mut syst, clocks.sysclk().to_Hz());

        // GPIO
        let gpioa= dp.GPIOA.split();
        let mut led = gpioa.pa5.into_push_pull_output();
        let.set_low();

        // Debug USART2 (PA2 TX, PA3 RX, AF7), 9600
        let tx2 = gpioa.pa2.into_alternate_af7();
        let rx2 = gpioa.pa3.into_alternate_af7();
        let (mut debug_tx, _) = hal::serial::Serial::usart2(
            dp.USART2, (tx2, rx2), Config::default().baudrate(9_600.bps()), &clocks)
            .unwrap().split();
        
            let _ = writeln!(debug_tx, "Boot: RS232 poller starting...");

        // Product USART1 (PA9 TX, PA10 RX, AF7), 1152000
        let tx1 = gpioa.pa9.into_alternate_af7();
        let rx1 = gpioa.pa10.into_alternate_af7();
        let serial1 = hal::serial::Serial::usart1(
            dp.USART1, (tx1, rx1), Config::default().baudrate(115_200.bps()), &clocks)
            .unwrap().split();
        
        let (mut product_tx, mut product_rx) = serial1;

        // Enable RXNE interrupt on USART1
        product_rx.listen(Event::Rxne);
        
        // Start task
        heartbeat_task::spawn().ok();
        polling_task::spawn().ok();
        (
            Shared {telemetry: TelemetryData::default(), req_index: 0},
            Local{
                led,
                product_tx,
                product_rx,
                debug_tx,
                tx_buf: Vec::new(),
                rx_line: String::new(),
                awaiting_resp: false,
            },
            init::Monotonics()
        )
    }

    // HEARTBEAT - LED
    #[task(local = [led])]
    fn heartbeat_task(cx: heartbeat_task::Context){
        cx.local.led.toggle();
        heartbeat_task::spawn_after(HEARTBEAT_INTERVAL.millis()),ok();
    }
    
    // POLLING SCHEDULER
    #[task(shared = [req_index], local = [product_tx, tx_buf], priority = 2)]
    fn polling_task(mut cx: polling_task::Context){
        // round-robin request list
        let requests: &[Request] = &[
            Request::ReadAc,
            Request::ReadVout,
            Request::ReadIout,
            Request::ReadBattV,
            Request::ReadBattI,
            Request::ReadTemps,
            Request::ReadState,
        ];

        let idx = *cx.shared.req_index;
        let req = requests[idx as usize % requests.len()];

        // Encode and send
        encode_request(req, cx.local.tx_buf);
        for b in cx.local.tx_buf.iter(){
            // Spin until sent
            nb::block!(cx.local.product_tx.write(*b)).ok();
        }

        *cx.shared.req_index = (idx +1) %(requests.len() as u8);
        response_timeout::spawn_after(RESP_TIMEOUT_MS.millis()).ok();
        polling_task::spawn_after(POLLING_INTERVAL.millis()).ok();
    }

    // --- response timeout ---
    #[task(local) = [awaiting_resp], priority = 2]
    fn response_timeout(cx: response_timeout::Context){
        if *cx.local.awaiting_resp
        // can trigger retry or raise an alarm
        *cx.local.awaiting_resp = false;
    }


    // USART1 interrupt handlers
    #[task(binds = USART1, local = [product_rx, rx_line, awaiting_resp], shared = [telemetry], priority =3)]
    fn usart1_rx(mut cx: usart1_rx::Context){
        while let Ok(byte) = cx.local.product_rx.read(){
            let ch = byte as char;
            if ch == '\n' {
                // assuming CRLF parse it - ful line
                let line = cx.local.rx_line.as_str();
                cx.local.awaiting_resp = false;

                cx.shared.telemetry.lock(|tel| {
                    parse_line(line, tel);
                });

                //clear buffer
                cx.local.rx_line.clear();
            }else if ch != '\r'{
                // Accumulate - bounded
                if cx.local.rx_line.push(ch).is_err(){
                    // overflow - reset line
                    cx.local.rx_line.clear();
                }
            }
        }
    }


    // Dump telemetry periodically
    #[task(shared = [telemetry], local = [debug_tx], priority =1)]
    fn dump_telemetry(mut cx: dump_telemetry::Contex){
        cx.shared.telemetry.lock(|t|{
            let _ = writeln!(
                cx.local.debug.tx,
                "AC {:.1}Vac {:.2}Hz PF={:?} | DC {:.2}V {:.2}A | Batt {:.2}V {:.2}A T(b)={:?} T(c)={:?}",
                t.ac_voltage_rms, t.ac_frequency_hz, t.power_factor,
                t.dc_output_voltage, t.dc_output_current,
                t.battery_voltage, t.battery_current,
                t.battery_temperature_c, t.charger_temperature_c,
            );
        });
        dump_telemetry::spawn_after(1000_u32.millis()).ok();

    }
}
  