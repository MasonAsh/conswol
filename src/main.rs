extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate toml;
extern crate tui;
extern crate termion;
extern crate failure;
extern crate regex;

use std::io;
use std::io::prelude::*;
use std::fs::File;
use std::path;
use std::sync::mpsc;
use std::sync::mpsc::{Sender, Receiver};
use std::path::PathBuf;
use std::collections::HashMap;

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

use regex::Regex;

use serde::Deserializer;

fn default_working_dir() -> String {
    String::from("./")
}

fn default_severity_mapper() -> HashMap<String, MessageSeverity> {
    let mut severity_mapper = HashMap::new();
    severity_mapper.insert(String::from("error"), MessageSeverity::Error);
    severity_mapper.insert(String::from("warning"), MessageSeverity::Warning);
    severity_mapper
}

#[derive(Serialize, Deserialize, Clone, Copy)]
enum MessageSeverity {
    Error,
    Warning,
    Other,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
struct CommandConfig {
    #[serde(default = "default_working_dir")]
    working_dir: String,
    command: String,
    args: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone)]
struct ProblemMatcher {
    regex: String,
    file_group: Option<u16>,
    line_group: Option<u16>,
    col_group: Option<u16>,
    severity_group: Option<u16>,
    severity_mapper: Option<HashMap<String, MessageSeverity>>,
}

#[derive(Serialize, Deserialize)]
struct Project {
    dir: String,
    build_cmd: Option<CommandConfig>,
    run_cmd: Option<CommandConfig>,
    problem_matcher: Option<ProblemMatcher>,
}

enum MainWindow {
    ErrorList,
    Shell,
}

struct LineCol(u32, u32);

struct CompilerMessage {
    severity: Option<MessageSeverity>,
    line: Option<u32>,
    col: Option<u32>,
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


fn execute_build_cmd(build_cmd: CommandConfig, problem_matcher: &Option<ProblemMatcher>, tx: Sender<BuildState>) {
    use std::process::{Command, Output};
    use std::thread;

    fn read_compiler_messages(output: &Output, problem_matcher: &Option<ProblemMatcher>) -> Result<Vec<CompilerMessage>, Error> {
        let mut messages = Vec::new();

        let stdout = output.stdout.to_owned();
        let stdout = String::from_utf8(stdout)?;

        let ref stderr = output.stderr;
        let stderr = std::str::from_utf8(stderr)?;

        let combined_output = stdout + stderr;

        if let Some(problem_matcher) = problem_matcher {
            let regex = Regex::new(problem_matcher.regex.as_str())?;
            let captures: Vec<regex::Captures> = regex.captures_iter(combined_output.as_str()).collect();
            for i in 0..captures.len() {
                let capture = captures.get(i);
                if capture.is_none() {
                    continue;
                }
                let capture = capture.unwrap();

                let full_match = capture.get(0).unwrap();
                let message_start = full_match.start();
                let message_end = if i < captures.len() - 1 {
                    // The end of this message should be the start index of the next message
                    captures.get(i+1).unwrap().get(0).unwrap().start()
                } else {
                    // Otherwise if no other captures just to the end of the output.
                    combined_output.len()
                };

                let file = if let Some(group) = problem_matcher.file_group {
                    match capture.get(group as usize) {
                        Some(ma) => Some(PathBuf::from(&combined_output[ma.start() .. ma.end()])),
                        None => None
                    }
                } else {
                    None
                };

                let content = &combined_output[message_start .. message_end];
                let content = content.to_string();

                let line = if let Some(group) = problem_matcher.line_group {
                    match capture.get(group as usize) {
                        Some(ma) => {
                            let cap_text = &combined_output[ma.start() .. ma.end()];
                            cap_text.parse::<u32>().ok()
                        },
                        None => None
                    }
                } else {
                    None
                };

                let col = if let Some(group) = problem_matcher.col_group {
                    match capture.get(group as usize) {
                        Some(ma) => {
                            let cap_text = &combined_output[ma.start() .. ma.end()];
                            cap_text.parse::<u32>().ok()
                        },
                        None => None
                    }
                } else {
                    None
                };

                let severity = if let Some(group) = problem_matcher.severity_group {
                    let ref severity_mapper = problem_matcher.severity_mapper;
                    match capture.get(group as usize) {
                        Some(ma) => {
                            let cap_text = &combined_output[ma.start() .. ma.end()];
                            if let Some(severity_mapper) = severity_mapper {
                                Some(severity_mapper.get(cap_text).unwrap().to_owned())
                            } else {
                                match cap_text.to_lowercase().as_str() {
                                    "error" => Some(MessageSeverity::Error),
                                    "warning" => Some(MessageSeverity::Warning),
                                    _ => None
                                }
                            }
                        },
                        None => None
                    }
                } else {
                    None
                };

                messages.push(CompilerMessage {
                    severity,
                    line,
                    col,
                    file,
                    content,
                });
            }
        } else {
            // No problem matcher, so just plain show the output.
            messages.push(CompilerMessage {
                severity: None,
                line: None,
                col: None,
                file: None,
                content: combined_output
            });
        }

        Ok(messages)
    }

    let problem_matcher = problem_matcher.clone();

    thread::spawn(move || {
        tx.send(BuildState::InProgress).unwrap();

        let command_res = Command::new(build_cmd.command)
            .args(build_cmd.args.iter())
            .current_dir(build_cmd.working_dir)
            .output();


        match command_res {
            Ok(output) => {
                let messages = read_compiler_messages(&output, &problem_matcher).unwrap_or_default();
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

    // FIXME: SelectableList does not render newlines.
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

fn handle_build_request(&MainState{ ref project, ref build_state, .. } : &MainState) -> Option<Receiver<BuildState>> {
    match project.build_cmd {
        Some(ref build_cmd) => {
            match build_state {
                // Don't build if a build is in progress
                BuildState::InProgress => {None},
                _ => {
                    let builder_channel = mpsc::channel();
                    let builder_tx = builder_channel.0;
                    execute_build_cmd(build_cmd.clone(), &project.problem_matcher, builder_tx.clone());
                    Some(builder_channel.1)
                }
            }
        },
        None => None
    }
}

fn main() {
    use std::env::args;

    let args: Vec<String> = args().collect();

    let project_dir = if let Some(project_dir) = args.get(1) {
        project_dir
    } else {
        "./"
    };

    let stdout = io::stdout().into_raw_mode().expect("Failed to open stdout.");
    let stdout = AlternateScreen::from(stdout);
    let backend = TermionBackend::new(stdout);
    let mut terminal = Terminal::new(backend).expect("Failed to start the TUI");
    terminal.hide_cursor().unwrap();
    let size = terminal.size().expect("Failed to get terminal size");

    std::env::set_current_dir(project_dir).expect("failed to load project");
    let project = load_project(project_dir).unwrap();

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
                    builder_rx = handle_build_request(&main_state);
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
