use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, QueueableCommand};
use std::fs;
use std::io::{self, Write};
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
struct CpuTimes {
    idle: u64,
    total: u64,
}

fn read_cpu_times() -> Option<CpuTimes> {
    let contents = fs::read_to_string("/proc/stat").ok()?;
    let line = contents.lines().next()?;
    let mut parts = line.split_whitespace();
    if parts.next()? != "cpu" {
        return None;
    }

    let mut total: u64 = 0;
    let mut idle: u64 = 0;
    for (i, part) in parts.enumerate() {
        let value: u64 = part.parse().ok()?;
        total = total.saturating_add(value);
        if i == 3 || i == 4 {
            idle = idle.saturating_add(value);
        }
    }

    Some(CpuTimes { idle, total })
}

fn cpu_usage(prev: CpuTimes, next: CpuTimes) -> f32 {
    let total_delta = next.total.saturating_sub(prev.total);
    let idle_delta = next.idle.saturating_sub(prev.idle);
    if total_delta == 0 {
        return 0.0;
    }
    let usage = 1.0 - (idle_delta as f32 / total_delta as f32);
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
    value.max(0.0).min(1.0)
}

fn render(
    stdout: &mut io::Stdout,
    samples: &[f32],
    load: f32,
    phase: f32,
    pulse: f32,
    fps: u32,
) -> io::Result<()> {
    let (width, height) = terminal::size()?;
    if width == 0 || height == 0 {
        return Ok(());
    }

    let header_rows: u16 = 1;
    let footer_rows: u16 = 1;
    let plot_height = height.saturating_sub(header_rows + footer_rows) as usize;
    let plot_width = width as usize;
    if plot_height < 4 || plot_width < 10 {
        return Ok(());
    }

    let mut buffer = vec![vec![' '; plot_width]; plot_height];
    let mut prev_y: Option<usize> = None;

    for (x, &sample) in samples.iter().enumerate().take(plot_width) {
        let y = ((1.0 - sample) * (plot_height as f32 - 1.0)).round() as usize;
        let y = y.min(plot_height - 1);
        buffer[y][x] = '*';

        if let Some(prev) = prev_y {
            if prev != y {
                let (min_y, max_y) = if prev < y { (prev, y) } else { (y, prev) };
                for row in (min_y + 1)..max_y {
                    buffer[row][x] = '|';
                }
            }
        }
        prev_y = Some(y);
    }

    let header = format!(
        "CPU ECG  load: {:>5.1}%  fps: {:>2}  phase: {:>5.1}  pulse: {:>4.2}",
        load * 100.0,
        fps,
        phase,
        pulse
    );
    let footer = "Press Q/Esc to quit  +/- to change FPS";

    stdout.queue(Clear(ClearType::All))?;
    stdout.queue(MoveTo(0, 0))?;
    stdout.queue(SetForegroundColor(Color::White))?;
    stdout.queue(Print(truncate_to_width(&header, width)))?;

    stdout.queue(SetForegroundColor(line_color(load)))?;
    for (row, line) in buffer.into_iter().enumerate() {
        let y = header_rows + row as u16;
        stdout.queue(MoveTo(0, y))?;
        let line_string: String = line.into_iter().collect();
        stdout.queue(Print(line_string))?;
    }

    stdout.queue(SetForegroundColor(Color::DarkGrey))?;
    stdout.queue(MoveTo(0, height.saturating_sub(1)))?;
    stdout.queue(Print(truncate_to_width(footer, width)))?;
    stdout.queue(ResetColor)?;
    stdout.flush()?;
    Ok(())
}

fn truncate_to_width(text: &str, width: u16) -> String {
    let max = width as usize;
    text.chars().take(max).collect()
}

fn resize_samples(samples: &mut Vec<f32>, width: usize) {
    if samples.len() == width {
        return;
    }
    if samples.len() < width {
        let add = width - samples.len();
        samples.extend(std::iter::repeat(0.5).take(add));
    } else {
        let drop = samples.len() - width;
        samples.drain(0..drop);
    }
}

fn main() -> io::Result<()> {
    let mut stdout = io::stdout();
    terminal::enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen, Hide)?;

    let mut fps: u32 = 30;
    let fps_min: u32 = 10;
    let fps_max: u32 = 60;
    let mut phase: f32 = 0.0;
    let mut pulse: f32 = 0.0;
    let mut prev_cpu = read_cpu_times().unwrap_or(CpuTimes { idle: 0, total: 1 });
    let mut samples: Vec<f32> = Vec::new();
    let mut last_draw = Instant::now();
    let mut tick: u64 = 0;

    loop {
        let now = Instant::now();
        let elapsed = now.duration_since(last_draw);
        let tick_rate = Duration::from_millis(u64::from(1000 / fps.max(1)));
        if elapsed < tick_rate {
            if event::poll(tick_rate - elapsed)? {
                if let Event::Key(KeyEvent {
                    code, modifiers, ..
                }) = event::read()?
                {
                    if code == KeyCode::Char('q') || code == KeyCode::Esc {
                        break;
                    }
                    if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
                        break;
                    }
                    if code == KeyCode::Char('+') || code == KeyCode::Char('=') {
                        fps = (fps + 5).min(fps_max);
                    }
                    if code == KeyCode::Char('-') || code == KeyCode::Char('_') {
                        fps = fps.saturating_sub(5).max(fps_min);
                    }
                }
            }
            continue;
        }
        last_draw = now;

        let cpu = read_cpu_times().unwrap_or(prev_cpu);
        let load = cpu_usage(prev_cpu, cpu);
        prev_cpu = cpu;

        let (width, height) = terminal::size()?;
        let plot_width = width as usize;
        if height > 2 && plot_width > 0 {
            resize_samples(&mut samples, plot_width);

            phase += 0.25 + load * 0.7;
            if phase > 1000.0 {
                phase = 0.0;
            }

            if load > 0.7 && tick % 18 == 0 {
                pulse = 1.0;
            }
            pulse *= 0.65;

            let base = 0.5 + 0.35 * (phase).sin();
            let mut sample = base + pulse * 0.9;
            if load < 0.2 {
                sample = 0.5 + 0.2 * (phase * 0.7).sin();
            }
            let sample = clamp_sample(sample);
            samples.push(sample);
            if samples.len() > plot_width {
                samples.remove(0);
            }

            render(&mut stdout, &samples, load, phase, pulse, fps)?;
        }

        tick = tick.saturating_add(1);
    }

    execute!(stdout, Show, LeaveAlternateScreen)?;
    terminal::disable_raw_mode()?;
    Ok(())
}
