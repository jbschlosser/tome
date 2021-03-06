use context::Context;
use mio::tcp::TcpStream;
use scripting::{self, ScriptAction};
use std::fs::File;
use std::io;
use std::io::{Read, Write};
use std::path::PathBuf;
use session::Session;
use tome::{formatted_string, Style, Color, Format, FormattedString, RingBuffer,
    esc_seq, search, telnet, ParseState};

// Actions to be used directly for key bindings.
pub fn quit(_: &mut Context) -> bool { false }
pub fn prev_page(context: &mut Context) -> bool {
    let lines = context.viewport_lines / 2;
    context.current_session_mut().scrollback_buf.increment_index(lines);
    true
}
pub fn next_page(context: &mut Context) -> bool {
    let lines = context.viewport_lines / 2;
    context.current_session_mut().scrollback_buf.decrement_index(lines);
    true
}
pub fn backspace_input(context: &mut Context) -> bool {
    let cursor = context.cursor_index;
    if cursor > 0 {
        let index = context.history.index();
        context.history.data.get_recent_mut(index).remove(cursor - 1);
        context.cursor_index -= 1;
    }
    true
}
pub fn delete_input_char(context: &mut Context) -> bool {
    let input_len = context.history.data.get_recent(
        context.history.index()).len();
    let cursor = context.cursor_index;
    if cursor < input_len {
        let index = context.history.index();
        context.history.data.get_recent_mut(index).remove(cursor);
    }
    true
}
pub fn send_input(context: &mut Context) -> bool {
    // Check for an input hook. If one exists, run it; otherwise, just send
    // the contents of the input line.
    let input_line_contents = formatted_string::to_string(
        context.history.data.get_recent(context.history.index()));
    match context.script_interface.send_hook(&input_line_contents) {
        Ok(actions) => {
            actions.into_iter().map(|action| do_action(&action, context)).last();
        },
        Err(e) => {
            // Write the error to the scrollback buffer.
            write_scrollback(context,
                formatted_string::with_color(&e, Color::Red));
        }
    }

    // Add the input to the history and clear the input line.
    if context.history.index() > 0 {
        // History has been scrolled back and needs to be reset.
        context.history.reset_index();
        context.history.data.get_recent_mut(0).clear();
        write_to_line_buffer(
            &mut context.history.data,
            formatted_string::with_format(
                &format!("{}\n", &input_line_contents),
                Format::default()));
    } else {
        // Input line already contains the right data; just move
        // to the next line.
        context.history.data.push(FormattedString::new());
    }

    // Reset the cursor.
    context.cursor_index = 0;
    true
}
// Helper function to run a script action.
fn do_action(action: &ScriptAction, context: &mut Context) {
    match action {
        &ScriptAction::ReloadConfig => {
            reload_config(context);
        },
        &ScriptAction::WriteScrollback(ref fs) => {
            write_scrollback(context, fs.clone());
        },
        &ScriptAction::SendInput(ref s) => {
            send_data(context, &s, true);

            // Add to the scrollback buffer.
            write_scrollback(context,
                formatted_string::with_color(
                    &format!("{}\n", &s),
                    Color::Yellow));
        },
        &ScriptAction::Reconnect => {
            reconnect(context);
        },
        &ScriptAction::SearchBackwards(ref s) => {
            search_backwards(context, s)
        }
    }
}
pub fn cursor_left(context: &mut Context) -> bool {
    let cursor = context.cursor_index;
    if cursor > 0 {
        context.cursor_index -= 1;
    }
    true
}
pub fn cursor_right(context: &mut Context) -> bool {
    let input_len = context.history.data.get_recent(
        context.history.index()).len();
    let cursor = context.cursor_index;
    if cursor < input_len {
        context.cursor_index += 1;
    }
    true
}
pub fn history_prev(context: &mut Context) -> bool {
    context.history.increment_index(1);
    context.cursor_index = context.history.data.get_recent(
        context.history.index()).len();
    true
}
pub fn history_next(context: &mut Context) -> bool {
    context.history.decrement_index(1);
    context.cursor_index = context.history.data.get_recent(
        context.history.index()).len();
    true
}
pub fn delete_to_cursor(context: &mut Context) -> bool {
    let history_index = context.history.index();
    let curr_line = context.history.data.get_recent_mut(history_index);
    let after_cursor = curr_line.split_off(context.cursor_index);
    curr_line.clear();
    curr_line.extend(after_cursor);
    context.cursor_index = 0;
    true
}
pub fn reconnect(context: &mut Context) -> bool {
    match context.current_session().connection.peer_addr() {
        Ok(sa) => {
            match TcpStream::connect(&sa) {
                Ok(conn) => context.current_session_mut().connection = conn,
                Err(_) => () // TODO: Log this error.
            }
        },
        Err(_) => () // TODO: Log this error.
    }
    true
}
pub fn reload_config(context: &mut Context) -> bool {
    // Read the config file (if it exists).
    context.script_interface = scripting::init_interface();
    match read_file_contents(&context.config_filepath) {
        Ok(contents) => {
            if let Err(e) = context.script_interface.evaluate(&contents) {
                write_scrollback(context,
                    formatted_string::with_color(
                    &format!("Warning: config file error:\n{}\n", e),
                    Color::Yellow));
            }
        },
        Err(e) => {
            write_scrollback(context,
                formatted_string::with_color(
                    &format!("Warning: failed to read config file! ({})\n", e),
                    Color::Yellow));
        }
    }
    true
}
// Helper function to read a file's contents.
fn read_file_contents(filepath: &PathBuf) -> io::Result<String> {
    let mut file = try!(File::open(filepath));
    let mut file_contents = String::new();
    try!(file.read_to_string(&mut file_contents));
    Ok(file_contents)
}

// Actions with arguments.
pub fn write_scrollback(context: &mut Context, data: FormattedString) {
    write_to_line_buffer(
        &mut context.current_session_mut().scrollback_buf.data,
        data);
}
// Helper function to handle writing to buffers while being line-aware.
fn write_to_line_buffer(buffer: &mut RingBuffer<FormattedString>,
    data: FormattedString)
{
    for (ch, format) in data {
        match ch {
            '\r' => (),
            '\n' => buffer.push(FormattedString::new()),
            _ => buffer.get_recent_mut(0).push((ch, format))
        }
    }
}
pub fn send_data(context: &mut Context, data: &str, add_line_ending: bool) {
    // TODO: Check result.
    let data_to_send = format!("{}{}", data,
        if add_line_ending {"\r\n"} else {""});
    context.current_session_mut().connection.write(data_to_send.as_bytes());
}
pub fn insert_input_char(context: &mut Context, ch: char) {
    let hist_index = context.history.index();
    context.history.data.get_recent_mut(hist_index).insert(
        context.cursor_index, (ch, Format::default()));
    context.cursor_index += 1;
}
pub fn search_backwards(context: &mut Context, search_str: &str) {
    let viewport_lines = context.viewport_lines;
    let sess = context.current_session_mut();
    let start_line = match sess.prev_search_result {
        Some(p) => p.line_number + 1,
        None => 0
    };
    let this_result = match search::search_buffer(
        &sess.scrollback_buf.data, search_str, start_line)
    {
        Ok(r) => r,
        Err(_) => return // TODO: Add more.
    };

    // Un-highlight the old search result if there is one.
    if let Some(r) = sess.prev_search_result {
        let line = sess.scrollback_buf.data.get_recent_mut(r.line_number);
        highlight_string(line, r.begin_index, r.end_index, false);
    }

    // Highlight the new search result.
    if let Some(r) = this_result {
        let line_number = if r.line_number < viewport_lines {0}
            else {r.line_number - viewport_lines + 1};
        sess.scrollback_buf.set_index(line_number);
        let line = sess.scrollback_buf.data.get_recent_mut(r.line_number);
        highlight_string(line, r.begin_index, r.end_index, true);
    }

    // Store the new search result.
    sess.prev_search_result = this_result;
}
fn highlight_string(s: &mut FormattedString, start: usize, end: usize, on: bool) {
    for i in start..end {
        let (ch, format) = s[i];
        let mut new_format = format.clone();
        new_format.style = if on {Style::Standout} else {Style::Normal};
        s[i] = (ch, new_format);
    }
}
pub fn receive_data(context: &mut Context, data: &[u8]) {
    let string = handle_server_data(data, context.current_session_mut());
    match context.script_interface.recv_hook(&string) {
        Ok(actions) => {
            actions.into_iter().map(|action| do_action(&action, context)).last();
        },
        Err(e) => {
            // Write the error to the scrollback buffer.
            write_scrollback(context,
                formatted_string::with_color(&e, Color::Red));
        }
    }
}
// Helper function to deal with incoming data from the server.
fn handle_server_data(data: &[u8], session: &mut Session) -> FormattedString {
    let mut out_str = FormattedString::new();
    for byte in data {
        // Apply the telnet layer.
        let new_telnet_state = telnet::parse(&session.telnet_state, *byte);
        match new_telnet_state {
            ParseState::NotInProgress => {
                // Apply the esc sequence layer.
                let new_esc_seq_state =
                    esc_seq::parse(&session.esc_seq_state, *byte);
                match new_esc_seq_state {
                    ParseState::NotInProgress => {
                        // TODO: Properly convert to UTF-8.
                        out_str.push((*byte as char, session.char_format));
                    },
                    ParseState::InProgress(_) => (),
                    ParseState::Success(ref seq) => {
                        handle_esc_seq(&seq, session);
                    },
                    ParseState::Error(ref bad_seq) => {
                        warn!("Bad escape sequence encountered: {:?}", bad_seq);
                    }
                }
                session.esc_seq_state = new_esc_seq_state;
            },
            ParseState::InProgress(_) => (),
            ParseState::Success(ref cmd) => {
                info!("Telnet command encountered: {:?}", cmd);
                handle_telnet_cmd(&cmd, session);
            },
            ParseState::Error(ref bad_cmd) => {
                warn!("Bad telnet command encountered: {:?}", bad_cmd);
            }
        }
        session.telnet_state = new_telnet_state;
    }

    out_str
}
fn handle_telnet_cmd(cmd: &[u8], session: &mut Session) {
    // TODO: Implement this.
    if cmd.len() == 3 && &cmd[..3] == &[telnet::IAC, telnet::WILL, telnet::GMCP] {
        info!("IAC WILL GMCP received");
        session.connection.write(&[telnet::IAC, telnet::DO, telnet::GMCP]);
    }

    if cmd.len() > 3 && &cmd[..3] == &[telnet::IAC, telnet::SB, telnet::GMCP] {
        let mid: Vec<u8> = (&cmd[3..cmd.len() - 2])
            .iter()
            .map(|b| *b)
            .collect();
        let mid_str = match String::from_utf8(mid) {
            Ok(m) => m,
            Err(_) => return
        };
        info!("Received GMCP message: {}", &mid_str);
    }
}
fn handle_esc_seq(seq: &[u8], session: &mut Session) {
    // Use the esc sequence to update the char format for the session.
    let (style, fg_color, bg_color) = esc_seq::interpret(seq);
    if let Some(s) = style {
        session.char_format.style = s;
    }
    if let Some(f) = fg_color {
        session.char_format.fg_color = f;
    }
    if let Some(b) = bg_color {
        session.char_format.bg_color = b;
    }
}
