extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate toml;
extern crate tui;
extern crate termion;

use std::io;
use std::io::prelude::*;
use std::fs::File;
use std::path;
use std::sync::mpsc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tui::Terminal;
use tui::backend::TermionBackend;
use tui::widgets::{Widget, Block, Borders};
use tui::layout::{Layout, Constraint, Direction, Rect};
use tui::terminal::Frame;
use termion::raw::IntoRawMode;
use termion::event::Key;
use termion::input::TermRead;
use termion::screen::AlternateScreen;

fn default_working_dir() -> String {
    String::from("./")
}

#[derive(Serialize, Deserialize, Debug)]
struct CommandConfig {
    #[serde(default = "default_working_dir")]
    working_dir: String,
    command: String,
}

#[derive(Serialize, Deserialize)]
struct Project {
    dir: String,
    build_cmd: Option<CommandConfig>,
    run_cmd: Option<CommandConfig>,
}

enum MainWindow {
    ErrorList,
    Shell,
}

enum MessageType {
    Error,
    Warning,
    Other,
}

struct LineCol(u32, u32);

struct CompilerMessage {
    message_type: MessageType,
    line_col: Option<LineCol>,
}

struct BuildResults {
    ret_code: u32,
    messages: Vec<CompilerMessage>,
}

struct MainState {
    project: Project,
    main_window: MainWindow,
    build_results: Option<BuildResults>,
}

fn load_project(dir: &str) -> Option<Project> {
    use toml::Value;

    let project_config_path = path::PathBuf::from(dir).join("conswol.toml");

    let mut f = File::open(project_config_path);

    if let Ok(mut f) = f {
        let mut contents = String::new();
        f.read_to_string(&mut contents).expect("Failed to read project file");
        let project = toml::from_str::<Project>(contents.as_str());

        project.ok()
    } else {
        None
    }
}

fn draw_build_results_window<B>(mut frame: &mut Frame<B>, area: Rect, build_results: &Option<BuildResults>)
    where B: tui::backend::Backend {
    use tui::widgets::{Text, Paragraph};
    use tui::layout::Alignment;

    if let Some(_) = build_results {

    } else {
        let text = [Text::raw("Project is not built!")];
        Paragraph::new(text.iter())
            .block(Block::default().title("Build Results").borders(Borders::ALL))
            .alignment(Alignment::Center)
            .render(&mut frame, area);
    }
}

fn draw_shell_window<B>(mut frame: &mut Frame<B>, area: Rect) where B: tui::backend::Backend {
    Block::default()
        .title("Shell")
        .borders(Borders::ALL)
        .render(&mut frame, area);
}

fn main() {
    use std::env::args;

    let stdout = io::stdout().into_raw_mode().expect("Failed to open stdout.");
    let stdout = AlternateScreen::from(stdout);
    let backend = TermionBackend::new(stdout);
    let mut terminal = Terminal::new(backend).expect("Failed to start the TUI");
    let size = terminal.size().expect("Failed to get terminal size");

    let project = load_project("./").expect("Sometimes things just don't work.");

    let main_window = MainWindow::Shell;

    let mut main_state = MainState {
        project,
        main_window,
        build_results: None,
    };

    'mainloop: loop {
        terminal.draw(|mut f| {
            use tui::widgets::{Text, Paragraph};

            let chunks = Layout::default()
                .constraints([Constraint::Percentage(50), Constraint::Min(0)].as_ref())
                .direction(Direction::Vertical)
                .split(size);

            draw_build_results_window(&mut f, chunks[0], &main_state.build_results);
            draw_shell_window(&mut f, chunks[1])
        }).expect("Error rendering TUI");

        let stdin = io::stdin();
        for key in stdin.keys() {
            let key = key.expect("Failed to read stdin");
            match key {
                Key::Ctrl('c') => { break 'mainloop }
                _ => {}
            }
        }
    }
}
