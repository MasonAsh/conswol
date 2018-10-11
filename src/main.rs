extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate toml;
extern crate tui;
extern crate termion;
extern crate failure;

use std::io;
use std::io::prelude::*;
use std::fs::File;
use std::path;
use std::sync::mpsc;
use std::sync::mpsc::{Sender, Receiver};
use std::path::PathBuf;

use tui::Terminal;
use tui::backend::TermionBackend;
use tui::widgets::{Widget, Block, Borders};
use tui::layout::{Layout, Constraint, Direction, Rect};
use tui::terminal::Frame;
use termion::raw::IntoRawMode;
use termion::event::Key;
use termion::input::TermRead;
use termion::screen::AlternateScreen;

use failure::Error;

fn default_working_dir() -> String {
    String::from("./")
}

#[derive(Clone, Serialize, Deserialize, Debug)]
struct CommandConfig {
    #[serde(default = "default_working_dir")]
    working_dir: String,
    command: String,
    args: Vec<String>,
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
    file: Option<PathBuf>,
    content: String,
}

struct BuildResults {
    ret_code: i32,
    messages: Vec<CompilerMessage>,
}

enum BuildState {
    NoBuild,
    InProgress,
    InvocationFailed,
    Finished(BuildResults),
}

struct MainState {
    project: Project,
    main_window: MainWindow,
    build_state: BuildState,
    selected_message: Option<usize>,
}

fn load_project(dir: &str) -> Option<Project> {
    let project_config_path = path::PathBuf::from(dir).join("conswol.toml");

    let f = File::open(project_config_path);

    if let Ok(mut f) = f {
        let mut contents = String::new();
        f.read_to_string(&mut contents).expect("Failed to read project file");
        let project = toml::from_str::<Project>(contents.as_str());

        project.ok()
    } else {
        None
    }
}


fn execute_build_cmd(build_cmd: CommandConfig, tx: Sender<BuildState>) {
    use std::process::{Command, Output};
    use std::thread;

    fn read_compiler_messages(output: &Output) -> Result<Vec<CompilerMessage>, Error> {
        let mut messages = Vec::new();

        let stdout = output.stdout.to_owned();
        let stdout = String::from_utf8(stdout)?;

        let ref stderr = output.stderr;
        let stderr = std::str::from_utf8(stderr)?;

        let combined_output = stdout + stderr;

        // FIXME: Actually parse out errors from the compiler output.
        //        This is going to require some awareness of what compiler is being used.
        for line in combined_output.lines() {
            messages.push(CompilerMessage {
                message_type: MessageType::Other,
                line_col: None,
                file: None,
                content: line.to_string(),
            });
        }

        Ok(messages)
    }

    thread::spawn(move || {
        tx.send(BuildState::InProgress).unwrap();

        let command_res = Command::new(build_cmd.command)
            .args(build_cmd.args.iter())
            .output();


        match command_res {
            Ok(output) => {
                let messages = read_compiler_messages(&output).unwrap_or_default();
                let status = output.status;
                let build_result = BuildResults {
                    ret_code: status.code().unwrap(),
                    messages: messages,
                };
                let bs = BuildState::Finished(build_result);
                tx.send(bs).unwrap();
            },
            Err(_) => {
                let bs = BuildState::InvocationFailed;
                tx.send(bs).unwrap();
            }
        };
    });
}

fn draw_build_results_window<B>(mut frame: &mut Frame<B>, area: Rect, build_state: &BuildState, selected_message: Option<usize>)
    where B: tui::backend::Backend {
    use tui::widgets::{Text, Paragraph, SelectableList};

    let mut text = Vec::new();

    match build_state {
        BuildState::NoBuild => {
            text.push("Project is not built!");
        },
        BuildState::InProgress => {
            text.push("~~~Building~~~");
        },
        BuildState::InvocationFailed => {
            text.push("Failed to run the build command. Check the conswol.toml");
        },
        BuildState::Finished(BuildResults{messages, ..}) => {
            for message in messages {
                text.push(message.content.as_str());
            }
        },
    };

    SelectableList::default()
        .block(Block::default().title("Build Results").borders(Borders::ALL))
        .items(text.as_slice())
        .select(selected_message)
        .highlight_symbol(">")
        .render(&mut frame, area);
}

fn draw_shell_window<B>(mut frame: &mut Frame<B>, area: Rect) where B: tui::backend::Backend {
    Block::default()
        .title("Shell")
        .borders(Borders::ALL)
        .render(&mut frame, area);
}

fn spawn_key_listener(key_tx: Sender<Key>) {
    std::thread::spawn(move|| {
        let stdin = io::stdin();
        for key in stdin.keys() {
            key_tx.send(key.unwrap()).unwrap();
        }
    });
}

fn main() {
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
        build_state: BuildState::NoBuild,
        selected_message: None,
    };

    let mut builder_rx : Option<Receiver<BuildState>> = None;

    // println! doesn't exactly work in a tui app so we render this message at the bottom.
    let mut debug_message = String::new();

    // Keys are read on a different thread and sent back via the channel
    let (key_tx, key_rx): (Sender<Key>, Receiver<Key>) = mpsc::channel();
    spawn_key_listener(key_tx);

    let mut last_selection_idx = 0i32;

    'mainloop: loop {
        terminal.draw(|mut f| {
            use tui::widgets::{Text, Paragraph};
            use tui::layout::Alignment;

            let chunks = Layout::default()
                .constraints([Constraint::Percentage(50), Constraint::Min(0), Constraint::Length(5)].as_ref())
                .direction(Direction::Vertical)
                .split(size);

            draw_build_results_window(&mut f, chunks[0], &main_state.build_state, main_state.selected_message);
            draw_shell_window(&mut f, chunks[1]);

            let text = [Text::raw(debug_message.clone())];

            Paragraph::new(text.iter())
                .block(Block::default().title("Debug Message").borders(Borders::ALL))
                .alignment(Alignment::Center)
                .render(&mut f, chunks[2]);
        }).expect("Error rendering TUI");

        while let Ok(key) = key_rx.try_recv() {
            match key {
                Key::Ctrl('c') => { break 'mainloop },
                Key::Ctrl('b') => {
                    debug_message = String::from("Ctrl+b was pressed....");
                    match main_state.project.build_cmd {
                        Some(ref build_cmd) => {
                            let builder_channel = mpsc::channel();
                            let builder_tx = builder_channel.0;
                            execute_build_cmd(build_cmd.clone(), builder_tx.clone());
                            builder_rx = Some(builder_channel.1);
                        },
                        None => {
                            debug_message = String::from("no build_cmd");
                        }
                    }
                },
                Key::Up => {
                    last_selection_idx -= 1;
                },
                Key::Down => {
                    last_selection_idx += 1;
                },
                _ => {}
            }
        }

        if let BuildState::Finished(ref build_results) = main_state.build_state {
            let num_messages = build_results.messages.len();

            if num_messages == 0 {
                // no messages to select, this check needs to be here to avoid bounds errors
                main_state.selected_message = None;
            } else {
                if last_selection_idx < 0 {
                    // wrap around to the bottom
                    last_selection_idx = num_messages as i32 + last_selection_idx;
                } else {
                    // wrap to top if needed
                    last_selection_idx = last_selection_idx % num_messages as i32;
                }

                main_state.selected_message = Some(last_selection_idx as usize);
            }
        }

        if let Some(ref builder_rx_val) = builder_rx {
            let recv_res = builder_rx_val.recv();
            match recv_res {
                Ok(build_state) => {
                    main_state.build_state = build_state;
                },
                Err(_) => {
                    builder_rx = None;
                }
            }
        }
    }
}
