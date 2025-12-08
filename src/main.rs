use eframe::{self, egui}; 
use egui_plot::{Line, Plot, PlotPoints};
use serialport::{self, available_ports, SerialPortInfo}; 
use std::collections::{VecDeque, HashMap};
use std::io::{self, BufReader, BufRead};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};
use serde::{Deserialize};
use serde_json; 

const MAX_PLOT_POINTS: usize = 500;

// --- New Serial Configuration Enums for JSON Deserialization ---

#[derive(Deserialize, Debug, Clone)]
pub enum ConfigParity { None, Odd, Even }

#[derive(Deserialize, Debug, Clone)]
pub enum ConfigStopBits { One, Two }

#[derive(Deserialize, Debug, Clone)]
pub enum ConfigDataBits { Five, Six, Seven, Eight }

#[derive(Deserialize, Debug, Clone)]
pub enum ConfigFlowControl { None, Software, Hardware }


// --- New Serial Configuration Structure (Replaces individual fields) ---

#[derive(Deserialize, Debug, Clone)]
pub struct SerialConfig {
    pub baud_rate: u32,
    pub parity: ConfigParity,
    pub data_bits: ConfigDataBits,
    pub stop_bits: ConfigStopBits,
    pub flow_control: ConfigFlowControl,
    pub polling_interval_ms: u64,
}

// --- Updated Core Config Structures ---

#[derive(Deserialize, Debug, Clone)]
pub struct PlotConfig {
    pub name: String,
    pub data_key: String, 
    pub color: [f32; 3],  
    pub y_min: f64,
    pub y_max: f64,
}

#[derive(Deserialize, Debug, Clone)]
pub struct CommandConfig {
    pub command: String,
    pub plots: Vec<PlotConfig>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct AppConfig {
    pub product_type: String, 
    pub serial: SerialConfig, // New: Holds all serial port settings
    pub commands: Vec<CommandConfig>,
    pub product_parameters: HashMap<String, String>,
}

#[derive(Debug, Clone)]
struct PlotData {
    pub timestamp: f64,
    pub plot_name: String, 
    pub value: Option<f64>,
    pub raw_line: String,
}

pub struct PlotState {
    pub config: PlotConfig,
    pub data: VecDeque<[f64; 2]>,
}

// --- SerialPlotterApp Structure Update ---

pub struct SerialPlotterApp {
    available_ports: Vec<SerialPortInfo>,
    selected_port_name: String,
    
    is_connected: bool,
    port_handle: Option<thread::JoinHandle<()>>, 
    serial_tx: Option<Sender<bool>>,
    
    config_file_path: String,
    loaded_config: Option<AppConfig>, // Used to pass config to worker
    config_load_error: Option<String>,

    plot_states: Vec<PlotState>, 
    state_values: HashMap<String, String>,
    log_history: VecDeque<String>,
    app_tx_main: Sender<PlotData>, 
    app_rx: Receiver<PlotData>,
    start_time: Instant,
    
    // Removed: active_baud_rate and polling_interval_ms
    
    editable_params: HashMap<String, String>,
    params_tx: Option<Sender<HashMap<String, String>>>,
    
    user_cmd_tx: Option<Sender<String>>,
    // Removed: interval_tx
    command_input: String,
}

impl Default for SerialPlotterApp {
    fn default() -> Self {
        let (app_tx, app_rx) = channel();
        
        let ports = available_ports().unwrap_or_default();
        let selected_port_name = ports.first().map(|p| p.port_name.clone()).unwrap_or_default();

        SerialPlotterApp {
            available_ports: ports,
            selected_port_name,
            is_connected: false,
            port_handle: None,
            serial_tx: None,
            
            config_file_path: "config.json".to_owned(),
            loaded_config: None,
            config_load_error: None,
            
            plot_states: Vec::new(),
            state_values: HashMap::new(),
            log_history: VecDeque::new(),
            app_tx_main: app_tx, 
            app_rx,
            start_time: Instant::now(),

            editable_params: HashMap::new(),
            params_tx: None,
            
            user_cmd_tx: None,
            command_input: String::new(),
        }
    }
}

impl SerialPlotterApp {
    pub fn load_config(&mut self) {
        self.config_load_error = None;
        self.loaded_config = None;
        self.plot_states.clear();
        self.state_values.clear(); 
        
        match std::fs::read_to_string(&self.config_file_path) {
            Ok(json_content) => {
                match serde_json::from_str::<AppConfig>(&json_content) {
                    Ok(config) => {
                        for cmd_cfg in config.commands.iter() {
                            for plot_cfg in cmd_cfg.plots.iter() {
                                self.plot_states.push(PlotState {
                                    config: plot_cfg.clone(),
                                    data: VecDeque::with_capacity(MAX_PLOT_POINTS),
                                });
                            }
                        }
                        
                        // All serial/polling settings are now loaded implicitly via 'config'
                        self.editable_params = config.product_parameters.clone();
                        self.loaded_config = Some(config);
                    },
                    Err(e) => {
                        self.config_load_error = Some(format!("JSON Parse Error: {}", e));
                    }
                }
            },
            Err(e) => {
                self.config_load_error = Some(format!("File Read Error: {}", e));
            }
        }
    }
}

// --- Helper Functions to Convert Config Enums to serialport Crate Enums ---

fn to_serial_parity(p: &ConfigParity) -> serialport::Parity {
    match p {
        ConfigParity::None => serialport::Parity::None,
        ConfigParity::Odd => serialport::Parity::Odd,
        ConfigParity::Even => serialport::Parity::Even,
    }
}

fn to_serial_stop_bits(s: &ConfigStopBits) -> serialport::StopBits {
    match s {
        ConfigStopBits::One => serialport::StopBits::One,
        ConfigStopBits::Two => serialport::StopBits::Two,
    }
}

fn to_serial_data_bits(d: &ConfigDataBits) -> serialport::DataBits {
    match d {
        ConfigDataBits::Five => serialport::DataBits::Five,
        ConfigDataBits::Six => serialport::DataBits::Six,
        ConfigDataBits::Seven => serialport::DataBits::Seven,
        ConfigDataBits::Eight => serialport::DataBits::Eight,
    }
}

fn to_serial_flow_control(f: &ConfigFlowControl) -> serialport::FlowControl {
    match f {
        ConfigFlowControl::None => serialport::FlowControl::None,
        ConfigFlowControl::Software => serialport::FlowControl::Software,
        ConfigFlowControl::Hardware => serialport::FlowControl::Hardware,
    }
}

// --- Serial Worker Function Update ---

fn serial_worker(
    port_name: String,
    serial_config: SerialConfig, // New: Full config struct passed here
    commands: Vec<CommandConfig>,
    app_tx: Sender<PlotData>,
    stop_rx: Receiver<bool>,
    cmd_rx: Receiver<String>,
    params_rx: Receiver<HashMap<String, String>>,
    initial_params: HashMap<String, String>,
) -> io::Result<()> {
    
    // Use settings from SerialConfig to initialize the port
    let port = serialport::new(&port_name, serial_config.baud_rate)
        .data_bits(to_serial_data_bits(&serial_config.data_bits))
        .parity(to_serial_parity(&serial_config.parity))
        .stop_bits(to_serial_stop_bits(&serial_config.stop_bits))
        .flow_control(to_serial_flow_control(&serial_config.flow_control))
        .timeout(Duration::from_millis(10))
        .open()?;

    let mut port_clone = port.try_clone()?;
    let mut reader = BufReader::new(port);
    let mut line_buffer = String::new();
    let start_time = Instant::now();
    let mut last_command_time = Instant::now();
    let polling_interval_ms = serial_config.polling_interval_ms; // Fixed interval from config
    
    let mut _product_parameters = initial_params; 
    
    loop {
        if stop_rx.try_recv().is_ok() {
            break;
        }
        
        // Removed: interval_rx handling

        while let Ok(new_params) = params_rx.try_recv() {
            _product_parameters = new_params;
            // NOTE: Logic to send 'set' commands based on new parameters would go here.
        }

        if last_command_time.elapsed() >= Duration::from_millis(polling_interval_ms) {
            for config in commands.iter() {
                let command_bytes = format!("{}\r\n", config.command).into_bytes();
                if port_clone.write_all(&command_bytes).is_err() {
                    eprintln!("Write error, exiting worker.");
                    return Ok(());
                }
            }
            last_command_time = Instant::now();
        }

        while let Ok(user_command) = cmd_rx.try_recv(){
            let command_bytes = format!("{}\r\n", user_command).into_bytes();
            if port_clone.write_all(&command_bytes).is_err(){
                eprintln!("User command write error.");
                return Ok(());
            }
        }

        while reader.read_line(&mut line_buffer).is_ok() {
            let current_time = start_time.elapsed().as_secs_f64();
            let raw_line = line_buffer.trim().to_owned();
            let mut is_recognized_data = false;
            
            for cmd_cfg in commands.iter() {
                for plot_cfg in cmd_cfg.plots.iter() {
                    if let Some((_, val_str)) = raw_line.split_once(&plot_cfg.data_key) {
                        
                        let val_str_trimmed = val_str.trim();
                        let mut plot_data = PlotData {
                            timestamp: current_time,
                            plot_name: plot_cfg.name.clone(), 
                            value: None, 
                            raw_line: raw_line.clone(),
                        };

                        if let Ok(value) = val_str_trimmed.parse::<f64>() {
                            plot_data.value = Some(value);
                        } 

                        if app_tx.send(plot_data).is_err() { return Ok(()); }
                        is_recognized_data = true;
                    }
                }
            }
            
            if !is_recognized_data {
                if app_tx.send(PlotData { 
                    timestamp: current_time, 
                    plot_name: String::new(), 
                    value: None, 
                    raw_line: raw_line.clone(),
                }).is_err() { return Ok(()); }
            }
            
            line_buffer.clear();
        }
        
        thread::sleep(Duration::from_millis(1));
    }
    
    Ok(())
}

impl eframe::App for SerialPlotterApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        
        while let Ok(data) = self.app_rx.try_recv() {
            
            if self.log_history.len() >= 100 {
                self.log_history.pop_front();
            }
            self.log_history.push_back(data.raw_line.clone());
            
            match data.value {
                Some(value) => {
                    if let Some(state) = self.plot_states.iter_mut().find(|s| s.config.name == data.plot_name) {
                        let point: [f64; 2] = [data.timestamp, value];
                        if state.data.len() >= MAX_PLOT_POINTS {
                            state.data.pop_front();
                        }
                        state.data.push_back(point);
                    }
                },
                None => {
                    if let Some(config) = self.plot_states.iter().find(|s| s.config.name == data.plot_name) {
                        if let Some((_, value)) = data.raw_line.split_once(&config.config.data_key) {
                            self.state_values.insert(data.plot_name.clone(), value.trim().to_owned());
                        }
                    }
                }
            }
            
            ctx.request_repaint();
        }
        
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading(egui::RichText::new("Rust Multi-Plot Serial Monitor (egui)").strong().size(20.0));
            ui.separator();
            
            // --- START: Main Columns Layout (Left for Controls, Right for Data) ---
            ui.columns(2, |columns| {
                
                // COLUMN 1: Configuration and Control Panels (Left Side)
                columns[0].vertical(|ui| {
                    ui.set_max_width(350.0); // Set a reasonable width for the control panels
                    egui::ScrollArea::vertical()
                        .id_source("control_scroll")
                        .show(ui, |ui| {
                            self.draw_config_panel(ui);
                            ui.separator();
                            self.draw_control_panel(ui);
                        });
                });

                // COLUMN 2: Plots and Logs (Right Side)
                columns[1].vertical(|ui| {
                    
                    // Nested Columns for Plots/States and Logs
                    ui.columns(2, |sub_columns| {
                        // Sub-Column 1: Real-Time Plots
                        sub_columns[0].vertical(|ui| {
                            ui.heading("Real-Time Data Plots");
                            egui::ScrollArea::vertical()
                                .id_source("plot_scroll_area")
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    for state in self.plot_states.iter().filter(|s| !self.state_values.contains_key(&s.config.name)) {
                                         self.draw_plot(ui, state);
                                    }
                                });
                        });

                        // Sub-Column 2: Logs and Current States
                        sub_columns[1].vertical(|ui| {
                            self.draw_log_panel(ui);
                        });
                    });
                });
            });
            // --- END: Main Columns Layout ---
        });
    }
}

impl SerialPlotterApp {

    fn draw_config_panel(&mut self, ui: &mut egui::Ui) {
        // Use a slight frame to visually separate the configuration
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.add_space(5.0);
            ui.heading("⚙️ Configuration Panel");
            ui.add_space(5.0);
            
            // 1. Config File and Load
            ui.horizontal(|ui| {
                ui.strong("Config File Path:");
                ui.text_edit_singleline(&mut self.config_file_path);

                if ui.button(egui::RichText::new("Load Config").strong()).clicked() {
                    self.load_config();
                }
                
                // Status feedback using RichText for color coding
                if let Some(err) = &self.config_load_error {
                    ui.label(egui::RichText::new(format!("❌ Error: {}", err)).color(egui::Color32::RED));
                } else if let Some(config) = &self.loaded_config {
                    ui.label(egui::RichText::new(format!("✅ Product: {}", config.product_type)).color(egui::Color32::GREEN));
                } else {
                    ui.label(egui::RichText::new("⚠️ Awaiting config.json load").color(egui::Color32::YELLOW));
                }
            });

            // 2. Display Static Connection Settings from Config (Read-Only)
            ui.add_space(10.0);
            ui.separator();
            ui.add_space(10.0);
            
            ui.heading("Static Serial Settings (from config.json)");
            
            if let Some(config) = &self.loaded_config {
                egui::Grid::new("static_config_grid")
                    .num_columns(2)
                    .spacing([20.0, 4.0])
                    .show(ui, |ui| {
                        ui.strong("Baud Rate:"); ui.label(format!("{}", config.serial.baud_rate)); ui.end_row();
                        ui.strong("Polling Interval (ms):"); ui.label(format!("{}", config.serial.polling_interval_ms)); ui.end_row();
                        ui.strong("Parity:"); ui.label(format!("{:?}", config.serial.parity)); ui.end_row();
                        ui.strong("Data Bits:"); ui.label(format!("{:?}", config.serial.data_bits)); ui.end_row();
                        ui.strong("Stop Bits:"); ui.label(format!("{:?}", config.serial.stop_bits)); ui.end_row();
                        ui.strong("Flow Control:"); ui.label(format!("{:?}", config.serial.flow_control)); ui.end_row();
                    });
            } else {
                ui.label("No static serial configuration loaded.");
            }
            
            // --- REMOVED: Connection & Polling Settings GUI Block ---
            
            ui.add_space(10.0);
            ui.separator();
            ui.add_space(10.0);

            ui.heading("Product Parameters");

            if self.editable_params.is_empty() {
                ui.label("No editable parameters defined in config.");
            } else {
                
                let mut keys: Vec<String> = self.editable_params.keys().cloned().collect();
                keys.sort();
                
                let mut params_changed = false;
                
                egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                    egui::Grid::new("product_params_grid")
                        .num_columns(2)
                        .spacing([30.0, 8.0])
                        .show(ui, |ui| {
                            
                            for key in &keys { 
                                ui.strong(format!("{}:", key));
                                
                                if let Some(value) = self.editable_params.get_mut(key) {
                                    // FIX: Create TextEdit, chain .font(), then use ui.add()
                                    let text_edit = egui::TextEdit::singleline(value)
                                        .font(egui::TextStyle::Monospace);
                                    
                                    if ui.add(text_edit).changed() {
                                        params_changed = true;
                                    }
                                }
                                ui.end_row();
                            }
                        });
                });

                if params_changed {
                    if let Some(tx) = &self.params_tx {
                        let _ = tx.send(self.editable_params.clone());
                    }
                }
            }
        }); // End of Frame
    }

    fn draw_control_panel(&mut self, ui: &mut egui::Ui) {
        // Use a second frame to group connection controls
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.add_space(5.0);
            ui.heading("🔌 Connection and Commands");
            ui.add_space(5.0);

            ui.horizontal(|ui| {
                ui.strong("Serial Port:");
                if ui.button("🔄 Refresh Ports").clicked() {
                    self.available_ports = available_ports().unwrap_or_default();
                }

                let ports: Vec<String> = self.available_ports.iter().map(|p| p.port_name.clone()).collect();
                egui::ComboBox::from_label("")
                    .selected_text(&self.selected_port_name)
                    .show_ui(ui, |ui| {
                        for port_name in ports.iter() {
                            ui.selectable_value(&mut self.selected_port_name, port_name.clone(), port_name);
                        }
                    });
                
                ui.add_space(15.0);

                if self.is_connected {
                    if ui.button(egui::RichText::new("🛑 Disconnect").color(egui::Color32::RED).strong()).clicked() {
                        self.disconnect();
                    }
                } else {
                    let connect_enabled = self.loaded_config.is_some() && !self.selected_port_name.is_empty();
                    let connect_button = egui::Button::new(egui::RichText::new("▶ Connect").color(egui::Color32::GREEN).strong());
                    if ui.add_enabled(connect_enabled, connect_button).clicked() {
                        self.connect(ui.ctx().clone());
                    }
                }
            });

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(10.0);

            // Command input and status grid
            egui::Grid::new("command_status_grid")
                .num_columns(3)
                .spacing([20.0, 8.0])
                .show(ui, |ui| {
                    ui.strong("Send Command:");
                    ui.text_edit_singleline(&mut self.command_input);

                    let send_enabled = self.is_connected && !self.command_input.is_empty();
                    if ui.add_enabled(send_enabled, egui::Button::new("📤 Send")).clicked() {
                        if let Some(tx) = &self.user_cmd_tx {
                            let command_to_send = self.command_input.clone();
                            if tx.send(command_to_send).is_ok() {
                                self.command_input.clear();
                            }
                        }
                    }
                    ui.end_row();

                    // Status display
                    ui.strong("Status:");
                    let status_text = if self.is_connected {
                        egui::RichText::new("ONLINE").color(egui::Color32::GREEN).strong()
                    } else {
                        egui::RichText::new("OFFLINE").color(egui::Color32::RED).strong()
                    };
                    ui.label(status_text);
                    ui.end_row();
                });
        }); // End of Frame
    }

    fn draw_plot(&self, ui: &mut egui::Ui, state: &PlotState) {
        if state.data.is_empty() {
            ui.label(format!("{}: Waiting for data...", state.config.name));
            return;
        }

        let line_data: Vec<[f64; 2]> = state.data.iter().copied().collect();
        let color = egui::Color32::from_rgb(
            (state.config.color[0] * 255.0) as u8,
            (state.config.color[1] * 255.0) as u8,
            (state.config.color[2] * 255.0) as u8,
        );
        
        let line = Line::new(PlotPoints::new(line_data))
            .name(&state.config.name)
            .color(color);

        let x_max = state.data.back().map(|p| p[0]).unwrap_or(0.0);
        let x_min = (x_max - 10.0).max(0.0);

        let plot = Plot::new(&state.config.name)
            .height(200.0)
            .include_y(state.config.y_min) 
            .include_y(state.config.y_max)
            .include_x(x_min)              
            .include_x(x_max + 1.0)
            .legend(egui_plot::Legend::default()); // Added legend
            
        plot.show(ui, |plot_ui| {
            plot_ui.line(line);
        });
    }

    fn draw_log_panel(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.heading("Current Device States");
            if self.state_values.is_empty() {
                ui.label("Waiting for text state data...");
            } else {
                egui::Grid::new("state_grid")
                    .num_columns(2)
                    .spacing([20.0, 4.0])
                    .show(ui, |ui| {
                        let mut state_display_names: Vec<String> = self.state_values.keys().cloned().collect();
                        state_display_names.sort();
                        
                        for name in state_display_names {
                            if let Some(value) = self.state_values.get(&name) {
                                ui.strong(format!("{}:", name));
                                ui.monospace(value);
                                ui.end_row();
                            }
                        }
                    });
            }
        });
        
        ui.separator();

        ui.heading("Serial Log");
        egui::ScrollArea::vertical()
            .id_source("log_scroll_area")
            .max_height(ui.available_height())
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in self.log_history.iter() {
                    ui.monospace(line);
                }
            });
    }
}

impl SerialPlotterApp {
    
    // --- Connect Function Update ---
    fn connect(&mut self, ctx: egui::Context) {
        self.disconnect(); 

        let config = match self.loaded_config.as_ref() {
            Some(c) => c,
            None => {
                self.config_load_error = Some("Cannot connect: Configuration not loaded.".to_owned());
                return; 
            },
        };

        let port_name = self.selected_port_name.clone();
        let serial_config = config.serial.clone(); // Pass the full serial configuration
        let commands = config.commands.clone();
        let initial_params = self.editable_params.clone();

        let (serial_tx, stop_rx) = channel();
        let (cmd_tx_gui, cmd_rx_worker) = channel();
        // Removed interval channels
        let (params_tx_gui, params_rx_worker) = channel();

        self.serial_tx = Some(serial_tx);
        self.user_cmd_tx = Some(cmd_tx_gui);
        self.params_tx = Some(params_tx_gui);

        let app_tx_worker = self.app_tx_main.clone(); 

        let handle = thread::spawn(move || {
            match serial_worker(
                port_name.clone(), 
                serial_config, // Pass the new SerialConfig struct
                commands, 
                app_tx_worker, 
                stop_rx, 
                cmd_rx_worker, 
                params_rx_worker,
                initial_params,
            ) {
                Ok(_) => println!("Serial worker for {} finished gracefully.", port_name),
                Err(e) => eprintln!("Serial error on {}: {}", port_name, e),
            }
            ctx.request_repaint(); 
        });

        self.port_handle = Some(handle);
        self.is_connected = true;
        self.start_time = Instant::now();
    }

    fn disconnect(&mut self) {
        if let Some(tx) = self.serial_tx.take() {
            let _ = tx.send(true); 
        }
        
        if let Some(handle) = self.port_handle.take() {
            let _ = handle.join();
        }

        self.user_cmd_tx.take();
        // Removed interval_tx.take();
        self.params_tx.take();
        
        self.is_connected = false;
    }
}


fn main() -> Result<(), eframe::Error> {
    let mut app = SerialPlotterApp::default();
    app.load_config();
    
    let native_options = eframe::NativeOptions::default();

    eframe::run_native( 
        "Rust Multi-Plot Serial Monitor",
        native_options,
        Box::new(|_cc| Box::new(app) as Box<dyn eframe::App>),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_test_app() -> SerialPlotterApp {
        let (app_tx, app_rx) = channel();
        SerialPlotterApp {
            available_ports: Vec::new(),
            selected_port_name: String::new(),
            is_connected: false,
            port_handle: None,
            serial_tx: None,
            config_file_path: "test_config.json".to_owned(),
            loaded_config: None,
            config_load_error: None,
            plot_states: Vec::new(),
            state_values: HashMap::new(),
            log_history: VecDeque::new(),
            app_tx_main: app_tx,
            app_rx,
            start_time: Instant::now(),
            editable_params: HashMap::new(),
            params_tx: None,
            user_cmd_tx: None,
            command_input: String::new(),
        }
    }

    #[test]
    fn test_config_loading_and_state_setup() {
        let config_json = r#"{
            "product_type": "TEST-100",
            "serial": {
                "baud_rate": 9600,
                "parity": "None",
                "data_bits": "Eight",
                "stop_bits": "One",
                "flow_control": "None",
                "polling_interval_ms": 50
            },
            "commands":[
                {
                    "command": "get temp",
                    "plots":[
                        {
                            "name": "Temperature (C)",
                            "data_key": "temp=",
                            "color":[1.0, 0.0, 0.0],
                            "y_min": 0.0,
                            "y_max": 100.0
                        }
                    ]
                }
            ],
            "product_parameters": {
                "FAN_SPEED": "MEDIUM"
            }
        }"#;

        let temp_file_path = "test_config.json";
        // Create a temporary file for the test
        if std::fs::write(temp_file_path, config_json).is_err() {
            panic!("Failed to write temporary config file.");
        }

        let mut app = setup_test_app();
        app.config_file_path = temp_file_path.to_string();
        app.load_config();

        assert!(app.loaded_config.is_some(), "Config should be loaded.");
        let config = app.loaded_config.as_ref().unwrap();
        
        assert_eq!(config.serial.baud_rate, 9600, "Baud Rate should be set from config.");
        assert_eq!(config.serial.polling_interval_ms, 50, "Polling Interval should be set from config.");
        assert_eq!(format!("{:?}", config.serial.parity), "None", "Parity should be set from config.");

        assert_eq!(app.editable_params.len(), 1, "Should have one editable parameter initialized.");
        assert_eq!(app.editable_params.get("FAN_SPEED").unwrap(), "MEDIUM", "FAN_SPEED should load default value.");

        // Clean up the temporary file
        let _ = std::fs::remove_file(temp_file_path);
    }
}