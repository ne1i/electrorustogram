use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, QueueableCommand};
use std::io::{self, Write};
use std::time::{Duration, Instant};
use sysinfo::System;

const HEADER_ROWS: u16 = 1;
const FOOTER_ROWS: u16 = 1;
const LEFT_GUTTER: u16 = 5;
const GRID_ROW_STEP: usize = 4;
const GRID_COL_STEP: usize = 6;
const MIN_PLOT_HEIGHT: usize = 4;
const MIN_PLOT_WIDTH: usize = 10;
const MILLIS_PER_SEC: u64 = 1000;

const FPS_DEFAULT: u32 = 30;
const FPS_MIN: u32 = 10;
const FPS_MAX: u32 = 60;

const PHASE_DELTA_BASE: f32 = 0.25;
const PHASE_DELTA_LOAD_SCALE: f32 = 0.7;
const PHASE_WRAP: f32 = 1000.0;
const PERCENT_SCALE: f32 = 100.0;

const PULSE_LOAD_THRESHOLD: f32 = 0.7;
const PULSE_INTERVAL_TICKS: u64 = 18;
const PULSE_PEAK: f32 = 1.0;
const PULSE_DECAY: f32 = 0.65;
const PULSE_GAIN: f32 = 0.9;

const BASE_AMPLITUDE: f32 = 0.7;
const LOW_LOAD_THRESHOLD: f32 = 0.2;
const LOW_LOAD_AMPLITUDE: f32 = 0.4;
const LOW_LOAD_PHASE_SCALE: f32 = 0.7;

const SIGNAL_MIN: f32 = -1.0;
const SIGNAL_MAX: f32 = 1.0;
const SIGNAL_RANGE: f32 = SIGNAL_MAX - SIGNAL_MIN;
const START_TICK: u64 = 1;
const TAU: f32 = std::f32::consts::TAU;

const FOOTER_TEXT: &str = "Press Q/Esc to quit  +/- to change FPS";

fn read_cpu_usage(sys: &mut System) -> f32 {
    sys.refresh_cpu_all();
    let usage = sys.global_cpu_usage() / 100.0;
    usage.clamp(0.0, 1.0)
}

fn line_color(load: f32) -> Color {
    if load < 0.5 {
        Color::Green
    } else if load < 0.75 {
        Color::Yellow
    } else {
        Color::Red
    }
}

fn clamp_sample(value: f32) -> f32 {
    value.clamp(SIGNAL_MIN, SIGNAL_MAX)
}

struct RenderMetrics {
    load: f32,
    phase: f32,
    pulse: f32,
    fps: u32,
    phase_delta: f32,
}

fn render(
    stdout: &mut io::Stdout,
    samples: &[f32],
    metrics: RenderMetrics,
    full_clear: bool,
) -> io::Result<()> {
    let (width, height) = terminal::size()?;
    if width == 0 || height == 0 {
        return Ok(());
    }

    let plot_height = height.saturating_sub(HEADER_ROWS + FOOTER_ROWS) as usize;
    let plot_width = width.saturating_sub(LEFT_GUTTER) as usize;
    if plot_height < MIN_PLOT_HEIGHT || plot_width < MIN_PLOT_WIDTH {
        return Ok(());
    }

    let mut buffer = vec![vec![' '; plot_width]; plot_height];
    for row in (0..plot_height).step_by(GRID_ROW_STEP) {
        for col in (0..plot_width).step_by(GRID_COL_STEP) {
            buffer[row][col] = '.';
        }
    }
    let mut trace_points: Vec<(usize, usize, char)> = Vec::new();
    let mut prev_y: Option<usize> = None;

    for (x, &sample) in samples.iter().enumerate().take(plot_width) {
        let normalized = (sample - SIGNAL_MIN) / SIGNAL_RANGE;
        let y = ((1.0 - normalized) * (plot_height as f32 - 1.0)).round() as usize;
        let y = y.min(plot_height - 1);
        trace_points.push((x, y, '*'));

        match prev_y {
            Some(prev) if prev != y => {
                let (min_y, max_y) = if prev < y { (prev, y) } else { (y, prev) };
                for row in (min_y + 1)..max_y {
                    trace_points.push((x, row, '|'));
                }
            }
            _ => {}
        }
        prev_y = Some(y);
    }

    let osc_hz = if metrics.phase_delta > 0.0 {
        (metrics.phase_delta * metrics.fps as f32) / TAU
    } else {
        0.0
    };
    let header = format!(
        "CPU ECG  load: {:>5.1}%  fps: {:>2}  osc: {:>4.2}Hz  phase: {:>5.1}  pulse: {:>4.2}",
        metrics.load * PERCENT_SCALE,
        metrics.fps,
        osc_hz,
        metrics.phase,
        metrics.pulse
    );
    let footer = FOOTER_TEXT;

    if full_clear {
        stdout.queue(Clear(ClearType::All))?;
    }
    stdout.queue(MoveTo(0, 0))?;
    stdout.queue(SetForegroundColor(Color::White))?;
    stdout.queue(Print(pad_to_width(&header, width)))?;

    stdout.queue(SetForegroundColor(Color::DarkGrey))?;
    let axis_top = 0usize;
    let axis_mid = plot_height / 2;
    let axis_bottom = plot_height.saturating_sub(1);
    for (row, line) in buffer.into_iter().enumerate() {
        let y = HEADER_ROWS + row as u16;
        let gutter = if row == axis_top {
            " 1.0|"
        } else if row == axis_mid {
            " 0.0|"
        } else if row == axis_bottom {
            "-1.0|"
        } else {
            "     "
        };
        stdout.queue(MoveTo(0, y))?;
        stdout.queue(Print(gutter))?;
        stdout.queue(MoveTo(LEFT_GUTTER, y))?;
        let line_string: String = line.into_iter().collect();
        stdout.queue(Print(line_string))?;
    }

    stdout.queue(SetForegroundColor(line_color(metrics.load)))?;
    for (x, y, ch) in trace_points {
        let draw_y = HEADER_ROWS + y as u16;
        stdout.queue(MoveTo(LEFT_GUTTER + x as u16, draw_y))?;
        stdout.queue(Print(ch))?;
    }

    stdout.queue(SetForegroundColor(Color::DarkGrey))?;
    stdout.queue(MoveTo(0, height.saturating_sub(1)))?;
    stdout.queue(Print(pad_to_width(footer, width)))?;
    stdout.queue(ResetColor)?;
    stdout.flush()?;
    Ok(())
}

fn pad_to_width(text: &str, width: u16) -> String {
    let max = width as usize;
    let mut out: String = text.chars().take(max).collect();
    let len = out.chars().count();
    if len < max {
        out.extend(std::iter::repeat_n(' ', max - len));
    }
    out
}

fn resize_samples(samples: &mut Vec<f32>, width: usize, fill: f32) {
    if samples.len() == width {
        return;
    }
    if samples.len() < width {
        let add = width - samples.len();
        samples.extend(std::iter::repeat_n(fill, add));
    } else {
        let drop = samples.len() - width;
        samples.drain(0..drop);
    }
}

fn main() -> io::Result<()> {
    let mut stdout = io::stdout();
    terminal::enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen, Hide)?;

    let mut fps: u32 = FPS_DEFAULT;
    let mut phase: f32 = 0.0;
    let mut pulse: f32 = 0.0;
    let mut sys = System::new();
    let mut samples: Vec<f32> = Vec::new();
    let mut last_draw = Instant::now();
    let mut tick: u64 = START_TICK;
    let mut last_size = terminal::size().unwrap_or((0, 0));
    let mut seeded = false;

    loop {
        let now = Instant::now();
        let elapsed = now.duration_since(last_draw);
        let tick_rate = Duration::from_millis(MILLIS_PER_SEC / u64::from(fps.max(1)));
        if elapsed < tick_rate {
            if event::poll(tick_rate - elapsed)?
                && let Event::Key(KeyEvent { code, modifiers, .. }) = event::read()?
            {
                if code == KeyCode::Char('q') || code == KeyCode::Esc {
                    break;
                }
                if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
                    break;
                }
                if code == KeyCode::Char('+') || code == KeyCode::Char('=') {
                    fps = (fps + 5).min(FPS_MAX);
                }
                if code == KeyCode::Char('-') || code == KeyCode::Char('_') {
                    fps = fps.saturating_sub(5).max(FPS_MIN);
                }
            }
            continue;
        }
        last_draw = now;

        let load = read_cpu_usage(&mut sys);

        let (width, height) = terminal::size()?;
        let plot_width = width.saturating_sub(LEFT_GUTTER) as usize;
        if height > HEADER_ROWS + FOOTER_ROWS && plot_width > 0 {
            let phase_delta = PHASE_DELTA_BASE + load * PHASE_DELTA_LOAD_SCALE;
            if load > PULSE_LOAD_THRESHOLD && tick.is_multiple_of(PULSE_INTERVAL_TICKS) {
                pulse = PULSE_PEAK;
            }
            pulse *= PULSE_DECAY;

            let base = BASE_AMPLITUDE * phase.sin();
            let mut sample = base + pulse * PULSE_GAIN;
            if load < LOW_LOAD_THRESHOLD {
                sample = LOW_LOAD_AMPLITUDE * (phase * LOW_LOAD_PHASE_SCALE).sin();
            }
            let sample = clamp_sample(sample);
            phase += phase_delta;
            if phase > PHASE_WRAP {
                phase = 0.0;
            }
            let full_clear = (width, height) != last_size;
            if full_clear {
                last_size = (width, height);
            }
            if !seeded {
                samples.clear();
                samples.resize(plot_width, sample);
                seeded = true;
            } else {
                let fill = samples.last().copied().unwrap_or(sample);
                resize_samples(&mut samples, plot_width, fill);
                samples.push(sample);
                if samples.len() > plot_width {
                    samples.remove(0);
                }
            }

            let metrics = RenderMetrics {
                load,
                phase,
                pulse,
                fps,
                phase_delta,
            };
            render(&mut stdout, &samples, metrics, full_clear)?;
        }

        tick = tick.saturating_add(1);
    }

    execute!(stdout, Show, LeaveAlternateScreen)?;
    terminal::disable_raw_mode()?;
    Ok(())
}
