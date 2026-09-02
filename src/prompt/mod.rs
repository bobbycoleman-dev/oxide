pub mod integration;

use crate::config::schema::{CwdStyle, PromptConfig, SegmentConfig, SegmentKind};
use crate::config::theme::{hsla_to_rgb8, parse_hex};

/// "r;g;b" for SGR 38;2/48;2 parameters.
fn sgr_rgb(hex: Option<&str>, fallback: (u8, u8, u8)) -> String {
    let (r, g, b) = hex
        .and_then(parse_hex)
        .map(hsla_to_rgb8)
        .unwrap_or(fallback);
    format!("{r};{g};{b}")
}

const DEFAULT_FG: (u8, u8, u8) = (17, 17, 27);
const DEFAULT_BG: (u8, u8, u8) = (137, 180, 250);

/// Escape a string for inclusion inside zsh double quotes.
fn zsh_dq(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`")
}

fn segment_snippet(ix: usize, seg: &SegmentConfig) -> String {
    let fg = sgr_rgb(seg.fg.as_deref(), DEFAULT_FG);
    let bg = sgr_rgb(seg.bg.as_deref(), DEFAULT_BG);
    let bold = if seg.bold { "1" } else { "0" };
    let opts = &seg.options;
    match seg.kind {
        SegmentKind::Cwd => {
            let max_len = opts.max_len.unwrap_or(40).max(4);
            let style = opts.style.unwrap_or(CwdStyle::TruncateToRepo);
            let compute = match style {
                CwdStyle::Full => "local __cwd=\"${(%):-%~}\"\n".to_string(),
                CwdStyle::Basename => "local __cwd=\"${PWD:t}\"\n".to_string(),
                CwdStyle::TruncateToRepo => concat!(
                    "local __cwd=\"${(%):-%~}\"\n",
                    "local __root=\"\"\n(( __oxide_git_ok )) && __root=$(command git rev-parse --show-toplevel 2>/dev/null)\n",
                    "if [[ -n \"$__root\" && \"$PWD\" == \"$__root\"* ]]; then __cwd=\"${__root:t}${PWD#$__root}\"; fi\n",
                )
                .to_string(),
            };
            format!(
                "{compute}(( ${{#__cwd}} > {max_len} )) && __cwd=\"…${{__cwd: -{max_len}}}\"\n\
                 __oxide_seg \"$__cwd\" \"{fg}\" \"{bg}\" {bold}\n"
            )
        }
        SegmentKind::Git => {
            let dirty_bg = sgr_rgb(opts.dirty_bg.as_deref(), (249, 226, 175));
            let show_dirty = opts.show_dirty.unwrap_or(true);
            let ahead_behind = opts.ahead_behind.unwrap_or(true);
            let mut s = String::new();
            s.push_str(
                "local __br=\"\"\n(( __oxide_git_ok )) && __br=$(command git symbolic-ref --short HEAD 2>/dev/null || command git rev-parse --short HEAD 2>/dev/null)\n",
            );
            s.push_str("if [[ -n \"$__br\" ]]; then\n");
            s.push_str(&format!("  local __gbg=\"{bg}\"\n"));
            // Literal UTF-8 bytes, not $'\ue0a0' — \u escapes fail with
            // "character not in range" when the shell runs under the C locale.
            s.push_str("  local __gtext=\"\u{e0a0} $__br\"\n");
            if show_dirty {
                s.push_str(
                    "  if [[ -n $(command git status --porcelain --untracked-files=no 2>/dev/null | command head -c1) ]]; then ",
                );
                s.push_str(&format!("__gbg=\"{dirty_bg}\"; fi\n"));
            }
            if ahead_behind {
                s.push_str("  local __ab=$(command git rev-list --left-right --count 'HEAD...@{upstream}' 2>/dev/null)\n");
                s.push_str("  if [[ -n \"$__ab\" ]]; then\n");
                s.push_str(
                    "    local __ahead=\"${__ab%%$'\\t'*}\" __behind=\"${__ab##*$'\\t'}\"\n",
                );
                s.push_str("    (( __ahead > 0 )) && __gtext+=\" ⇡$__ahead\"\n");
                s.push_str("    (( __behind > 0 )) && __gtext+=\" ⇣$__behind\"\n");
                s.push_str("  fi\n");
            }
            s.push_str(&format!(
                "  __oxide_seg \"$__gtext\" \"{fg}\" \"$__gbg\" {bold}\n"
            ));
            s.push_str("fi\n");
            s
        }
        SegmentKind::ExitStatus => {
            let hide = opts.hide_on_success.unwrap_or(true);
            if hide {
                format!(
                    "if (( __oxide_exit != 0 )); then __oxide_seg \"✗ $__oxide_exit\" \"{fg}\" \"{bg}\" {bold}; fi\n"
                )
            } else {
                format!("__oxide_seg \"$__oxide_exit\" \"{fg}\" \"{bg}\" {bold}\n")
            }
        }
        SegmentKind::Time => {
            let format = opts.format.clone().unwrap_or_else(|| "%H:%M".into());
            format!("__oxide_seg \"${{(%):-%D{{{format}}}}}\" \"{fg}\" \"{bg}\" {bold}\n")
        }
        SegmentKind::User => {
            format!("__oxide_seg \"${{(%):-%n}}\" \"{fg}\" \"{bg}\" {bold}\n")
        }
        SegmentKind::Host => {
            format!("__oxide_seg \"${{(%):-%m}}\" \"{fg}\" \"{bg}\" {bold}\n")
        }
        SegmentKind::Duration => {
            format!(
                "if [[ -n \"$__oxide_dur\" ]] && (( __oxide_dur >= 2.0 )); then\n\
                 \x20 local __ds\n\
                 \x20 if (( __oxide_dur >= 60 )); then __ds=\"$(( ${{__oxide_dur%%.*}} / 60 ))m$(( ${{__oxide_dur%%.*}} % 60 ))s\"; else __ds=$(printf '%.1fs' \"$__oxide_dur\"); fi\n\
                 \x20 __oxide_seg \"$__ds\" \"{fg}\" \"{bg}\" {bold}\n\
                 fi\n"
            )
        }
        SegmentKind::Text => {
            let text = zsh_dq(opts.text.as_deref().unwrap_or(""));
            format!("__oxide_seg \"{text}\" \"{fg}\" \"{bg}\" {bold}\n")
        }
        SegmentKind::Env => match &opts.var {
            Some(var) if var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => format!(
                "[[ -n \"${{{var}}}\" ]] && __oxide_seg \"${{{var}}}\" \"{fg}\" \"{bg}\" {bold} # segment {ix}\n"
            ),
            _ => String::new(),
        },
    }
}

/// Compile the `[prompt]` spec into a zsh init script: OSC 133 semantic prompt
/// markers plus a precmd that rebuilds PROMPT from powerline segments.
pub fn generate_init(prompt: &PromptConfig, style_prompt: bool) -> String {
    let mut segments = String::new();
    for (ix, seg) in prompt.segments.iter().enumerate() {
        segments.push_str(&indent_lines(&segment_snippet(ix, seg), "  "));
    }
    let sep = zsh_dq(&prompt.separator);
    let end = zsh_dq(if prompt.end.is_empty() {
        &prompt.separator
    } else {
        &prompt.end
    });
    let newline = if prompt.newline_before_input {
        "p+=$'\\n'"
    } else {
        ":"
    };

    let style_prompt_flag = if style_prompt { 1 } else { 0 };
    format!(
        r#"# Generated by Oxide — do not edit; regenerated from config.toml on launch.
[[ -o interactive ]] || return
zmodload zsh/datetime 2>/dev/null
autoload -Uz add-zsh-hook

typeset -g __oxide_style_prompt={style_prompt_flag}
typeset -g __oxide_exit=0
__oxide_git_ok=0
if command -v git >/dev/null 2>&1; then
  __oxide_git_ok=1
  # Apple's /usr/bin/git is an installer shim until the CLT exist; running it
  # would pop a GUI dialog from inside the prompt. Check without invoking it.
  if [ "$(uname)" = Darwin ] && [ "$(command -v git)" = /usr/bin/git ] \
    && ! /usr/bin/xcode-select -p >/dev/null 2>&1; then
    __oxide_git_ok=0
  fi
fi

typeset -g __oxide_dur=""
typeset -g __oxide_t0=""

__oxide_preexec() {{
  __oxide_t0=$EPOCHREALTIME
  printf '\033]133;C\033\\'
}}

__oxide_seg() {{
  __oxide_texts+=("$1"); __oxide_fgs+=("$2"); __oxide_bgs+=("$3"); __oxide_bolds+=("$4")
}}

__oxide_precmd() {{
  # Capture $? before anything else or we report our own exit status.
  __oxide_exit=$?
  if [[ -n "$__oxide_t0" ]]; then
    __oxide_dur=$(( EPOCHREALTIME - __oxide_t0 ))
  else
    __oxide_dur=""
  fi
  __oxide_t0=""
  printf '\033]133;D;%s\033\\' "$__oxide_exit"
  printf '\033]7;file://%s%s\033\\' "$HOST" "$PWD"

  local -a __oxide_texts __oxide_fgs __oxide_bgs __oxide_bolds
{segments}
  local sep="{sep}"
  local endc="{end}"
  local p=$'%{{\033]133;A\033\\%}}'
  local n=${{#__oxide_texts}} i
  for (( i=1; i<=n; i++ )); do
    local b=""
    [[ "${{__oxide_bolds[i]}}" == 1 ]] && b=$'\033[1m'
    # %{{...%}} zero-width markers keep zsh's printable-width math honest.
    p+="%{{"$'\033[38;2;'"${{__oxide_fgs[i]}}m"$'\033[48;2;'"${{__oxide_bgs[i]}}m${{b}}%}} ${{__oxide_texts[i]//\%/%%}} "
    if (( i < n )); then
      p+="%{{"$'\033[0m\033[38;2;'"${{__oxide_bgs[i]}}m"$'\033[48;2;'"${{__oxide_bgs[i+1]}}m%}}${{sep}}"
    else
      p+="%{{"$'\033[0m\033[38;2;'"${{__oxide_bgs[i]}}m%}}${{endc}}"
    fi
  done
  p+="%{{"$'\033[0m'"%}}"
  {newline}
  p+=" "
  p+=$'%{{\033]133;B\033\\%}}'
  (( n > 0 && __oxide_style_prompt )) && PROMPT="$p"
}}

add-zsh-hook precmd __oxide_precmd
add-zsh-hook preexec __oxide_preexec

# Silent cd: the app writes a target path and sends the trigger sequence.
# Running it as a zle widget (rather than typing a command) means nothing is
# echoed and reset-prompt redraws the existing prompt with the new directory.
__oxide_cd_widget() {{
  local f="${{HOME}}/.cache/oxide/cd_target" d
  if [[ -r $f ]]; then
    d="$(<$f)"
    [[ -d $d ]] && builtin cd -- "$d"
  fi
  zle reset-prompt
}}
zle -N __oxide_cd_widget
bindkey '\e[9001~' __oxide_cd_widget
"#
    )
}

fn bash_segment_snippet(seg: &SegmentConfig) -> String {
    let fg = sgr_rgb(seg.fg.as_deref(), DEFAULT_FG);
    let bg = sgr_rgb(seg.bg.as_deref(), DEFAULT_BG);
    let bold = if seg.bold { "1" } else { "0" };
    let opts = &seg.options;
    match seg.kind {
        SegmentKind::Cwd => {
            let max_len = opts.max_len.unwrap_or(40).max(4);
            let style = opts.style.unwrap_or(CwdStyle::TruncateToRepo);
            let compute = match style {
                CwdStyle::Full => "local __cwd=\"${PWD/#$HOME/\\~}\"\n".to_string(),
                CwdStyle::Basename => "local __cwd=\"${PWD##*/}\"\n".to_string(),
                CwdStyle::TruncateToRepo => concat!(
                    "local __cwd=\"${PWD/#$HOME/\\~}\"\n",
                    "local __root=\"\"\n(( __oxide_git_ok )) && __root=$(command git rev-parse --show-toplevel 2>/dev/null)\n",
                    "if [[ -n \"$__root\" && \"$PWD\" == \"$__root\"* ]]; then __cwd=\"${__root##*/}${PWD#$__root}\"; fi\n",
                )
                .to_string(),
            };
            format!(
                "{compute}(( ${{#__cwd}} > {max_len} )) && __cwd=\"…${{__cwd: -{max_len}}}\"\n\
                 __oxide_seg \"$__cwd\" \"{fg}\" \"{bg}\" {bold}\n"
            )
        }
        SegmentKind::Git => {
            let dirty_bg = sgr_rgb(opts.dirty_bg.as_deref(), (249, 226, 175));
            let show_dirty = opts.show_dirty.unwrap_or(true);
            let ahead_behind = opts.ahead_behind.unwrap_or(true);
            let mut s = String::new();
            s.push_str(
                "local __br=\"\"\n(( __oxide_git_ok )) && __br=$(command git symbolic-ref --short HEAD 2>/dev/null || command git rev-parse --short HEAD 2>/dev/null)\n",
            );
            s.push_str("if [[ -n \"$__br\" ]]; then\n");
            s.push_str(&format!("  local __gbg=\"{bg}\"\n"));
            s.push_str("  local __gtext=\"\u{e0a0} $__br\"\n");
            if show_dirty {
                s.push_str(
                    "  if [[ -n $(command git status --porcelain --untracked-files=no 2>/dev/null | command head -c1) ]]; then ",
                );
                s.push_str(&format!("__gbg=\"{dirty_bg}\"; fi\n"));
            }
            if ahead_behind {
                s.push_str("  local __ab=$(command git rev-list --left-right --count 'HEAD...@{upstream}' 2>/dev/null)\n");
                s.push_str("  if [[ -n \"$__ab\" ]]; then\n");
                s.push_str(
                    "    local __ahead=\"${__ab%%$'\\t'*}\" __behind=\"${__ab##*$'\\t'}\"\n",
                );
                s.push_str("    (( __ahead > 0 )) && __gtext+=\" ⇡$__ahead\"\n");
                s.push_str("    (( __behind > 0 )) && __gtext+=\" ⇣$__behind\"\n");
                s.push_str("  fi\n");
            }
            s.push_str(&format!(
                "  __oxide_seg \"$__gtext\" \"{fg}\" \"$__gbg\" {bold}\n"
            ));
            s.push_str("fi\n");
            s
        }
        SegmentKind::ExitStatus => {
            let hide = opts.hide_on_success.unwrap_or(true);
            if hide {
                format!(
                    "if (( __oxide_exit != 0 )); then __oxide_seg \"✗ $__oxide_exit\" \"{fg}\" \"{bg}\" {bold}; fi\n"
                )
            } else {
                format!("__oxide_seg \"$__oxide_exit\" \"{fg}\" \"{bg}\" {bold}\n")
            }
        }
        SegmentKind::Time => {
            let format = zsh_dq(&opts.format.clone().unwrap_or_else(|| "%H:%M".into()));
            format!("__oxide_seg \"$(command date +\"{format}\")\" \"{fg}\" \"{bg}\" {bold}\n")
        }
        SegmentKind::User => format!("__oxide_seg \"$USER\" \"{fg}\" \"{bg}\" {bold}\n"),
        SegmentKind::Host => {
            format!("__oxide_seg \"${{HOSTNAME%%.*}}\" \"{fg}\" \"{bg}\" {bold}\n")
        }
        SegmentKind::Duration => {
            format!(
                "if [[ -n \"$__oxide_dur\" ]] && (( __oxide_dur >= 2 )); then\n\
                 \x20 local __ds=\"${{__oxide_dur}}s\"\n\
                 \x20 (( __oxide_dur >= 60 )) && __ds=\"$(( __oxide_dur / 60 ))m$(( __oxide_dur % 60 ))s\"\n\
                 \x20 __oxide_seg \"$__ds\" \"{fg}\" \"{bg}\" {bold}\n\
                 fi\n"
            )
        }
        SegmentKind::Text => {
            let text = zsh_dq(opts.text.as_deref().unwrap_or(""));
            format!("__oxide_seg \"{text}\" \"{fg}\" \"{bg}\" {bold}\n")
        }
        SegmentKind::Env => match &opts.var {
            Some(var) if var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => format!(
                "[[ -n \"${{{var}}}\" ]] && __oxide_seg \"${{{var}}}\" \"{fg}\" \"{bg}\" {bold}\n"
            ),
            _ => String::new(),
        },
    }
}

/// Bash flavor of the init script, injected via `--init-file`. Emulates a
/// login shell (the flag is ignored by real login shells, so the caller strips
/// `-l`), then installs a PROMPT_COMMAND that rebuilds PS1 per prompt.
/// `\x01`/`\x02` are readline's zero-width markers (what `\[`/`\]` compile to).
pub fn generate_init_bash(prompt: &PromptConfig, style_prompt: bool) -> String {
    let mut segments = String::new();
    for seg in &prompt.segments {
        segments.push_str(&indent_lines(&bash_segment_snippet(seg), "  "));
    }
    let sep = zsh_dq(&prompt.separator);
    let end = zsh_dq(if prompt.end.is_empty() {
        &prompt.separator
    } else {
        &prompt.end
    });
    let newline = if prompt.newline_before_input {
        "p+=$'\\n'"
    } else {
        ":"
    };

    let style_prompt_flag = if style_prompt { 1 } else { 0 };
    format!(
        r#"# Generated by Oxide — do not edit; regenerated from config.toml on launch.
# Login-shell emulation: --init-file replaced -l, so run the profile chain.
if [[ -f /etc/profile ]]; then source /etc/profile; fi
if [[ -f "$HOME/.bash_profile" ]]; then source "$HOME/.bash_profile"
elif [[ -f "$HOME/.bash_login" ]]; then source "$HOME/.bash_login"
elif [[ -f "$HOME/.profile" ]]; then source "$HOME/.profile"
elif [[ -f "$HOME/.bashrc" ]]; then source "$HOME/.bashrc"
fi

[[ $- == *i* ]] || return 0

__oxide_style_prompt={style_prompt_flag}
__oxide_cd_erase=0
__oxide_git_ok=0
if command -v git >/dev/null 2>&1; then
  __oxide_git_ok=1
  # Apple's /usr/bin/git is an installer shim until the CLT exist; running it
  # would pop a GUI dialog from inside the prompt. Check without invoking it.
  if [ "$(uname)" = Darwin ] && [ "$(command -v git)" = /usr/bin/git ] \
    && ! /usr/bin/xcode-select -p >/dev/null 2>&1; then
    __oxide_git_ok=0
  fi
fi

__oxide_at_prompt=1
__oxide_t0=""
__oxide_dur=""
__oxide_exit=0
__oxide_original_prompt_command="$PROMPT_COMMAND"

# Note: this installs a DEBUG trap (the bash-preexec pattern) for OSC 133;C
# and command timing; a pre-existing DEBUG trap would be replaced.
__oxide_debug_trap() {{
  [[ -n "$COMP_LINE" ]] && return
  [[ "$BASH_COMMAND" == __oxide_prompt_command* ]] && return
  if (( __oxide_at_prompt )); then
    __oxide_at_prompt=0
    __oxide_t0=$SECONDS
    printf '\033]133;C\033\\'
  fi
}}
trap '__oxide_debug_trap' DEBUG

__oxide_seg() {{
  __oxide_texts+=("$1"); __oxide_fgs+=("$2"); __oxide_bgs+=("$3"); __oxide_bolds+=("$4")
}}

__oxide_prompt_command() {{
  # Capture $? before anything else or we report our own exit status.
  __oxide_exit=$?
  if [[ -n "$__oxide_t0" ]]; then __oxide_dur=$(( SECONDS - __oxide_t0 )); else __oxide_dur=""; fi
  __oxide_t0=""
  __oxide_at_prompt=1
  # Triggered by a silent cd: walk back over the prompt we just left behind
  # and clear it, so this prompt replaces it rather than stacking below.
  if (( __oxide_cd_erase > 0 )); then
    printf '\033[%dA\033[J' "$__oxide_cd_erase"
    __oxide_cd_erase=0
  fi
  printf '\033]133;D;%s\033\\' "$__oxide_exit"
  printf '\033]7;file://%s%s\033\\' "${{HOSTNAME:-localhost}}" "$PWD"
  if [[ -n "$__oxide_original_prompt_command" ]]; then
    ( exit "$__oxide_exit" )
    eval "$__oxide_original_prompt_command"
  fi

  local __oxide_texts=() __oxide_fgs=() __oxide_bgs=() __oxide_bolds=()
{segments}
  local sep="{sep}"
  local endc="{end}"
  local p=$'\001\033]133;A\033\\\002'
  local n=${{#__oxide_texts[@]}} i
  for (( i=0; i<n; i++ )); do
    local b=""
    [[ "${{__oxide_bolds[i]}}" == 1 ]] && b=$'\033[1m'
    local t="${{__oxide_texts[i]}}"
    t="${{t//\\/\\\\}}"; t="${{t//\$/\\\$}}"; t="${{t//\`/\\\`}}"
    p+=$'\001\033[38;2;'"${{__oxide_fgs[i]}}m"$'\033[48;2;'"${{__oxide_bgs[i]}}m$b"$'\002'" $t "
    if (( i+1 < n )); then
      p+=$'\001\033[0m\033[38;2;'"${{__oxide_bgs[i]}}m"$'\033[48;2;'"${{__oxide_bgs[i+1]}}m"$'\002'"$sep"
    else
      p+=$'\001\033[0m\033[38;2;'"${{__oxide_bgs[i]}}m"$'\002'"$endc"
    fi
  done
  p+=$'\001\033[0m\002'
  {newline}
  p+=" "
  p+=$'\001\033]133;B\033\\\002'
  (( n > 0 && __oxide_style_prompt )) && PS1="$p"
}}
PROMPT_COMMAND="__oxide_prompt_command"

# Silent cd. bash has no `zle reset-prompt`, so the app follows the trigger
# with an empty line to get a freshly expanded prompt. Record how tall the
# outgoing prompt is; the next __oxide_prompt_command erases it so the new
# prompt lands in place instead of stacking up.
__oxide_cd_widget() {{
  local f="${{HOME}}/.cache/oxide/cd_target" d
  if [[ -r $f ]]; then
    d="$(<$f)"
    [[ -d $d ]] && builtin cd -- "$d"
  fi
  if (( BASH_VERSINFO[0] >= 5 )); then
    # Count the outgoing prompt's lines so the erase covers all of them.
    # ${{PS1@P}} needs bash 4.4+; older bash assumes a one-line prompt.
    local __p="${{PS1@P}}" __nl
    __nl="${{__p//[!$'\n']/}}"
    __oxide_cd_erase=$(( ${{#__nl}} + 1 ))
  else
    __oxide_cd_erase=1
  fi
}}
if (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 3) )); then
  bind -x '"\e[9001~": __oxide_cd_widget' 2>/dev/null
else
  # bash < 4.3 cannot `bind -x` a multi-character sequence — invoking it dies
  # with "bash_execute_unix_command: cannot find keymap for command" (Apple's
  # /bin/bash 3.2 included). Trampoline through a two-key binding instead,
  # the same trick fzf uses.
  bind -x '"\C-x\C-a": __oxide_cd_widget' 2>/dev/null
  bind '"\e[9001~": "\C-x\C-a"' 2>/dev/null
fi
"#
    )
}

fn indent_lines(s: &str, prefix: &str) -> String {
    s.lines()
        .map(|l| {
            if l.is_empty() {
                l.to_string()
            } else {
                format!("{prefix}{l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::PromptConfig;

    #[test]
    fn generates_osc_markers_and_hooks() {
        let script = generate_init(&PromptConfig::default(), true);
        assert!(script.contains("133;A"));
        assert!(script.contains("133;B"));
        assert!(script.contains("133;C"));
        assert!(script.contains("133;D"));
        assert!(script.contains("add-zsh-hook precmd __oxide_precmd"));
        // Default config has cwd + git + exit segments.
        assert!(script.contains("__cwd"));
        assert!(script.contains("__br"));
        assert!(script.contains("__oxide_exit != 0"));
    }

    #[test]
    fn generated_scripts_pass_shell_syntax_check() {
        let dir = std::env::temp_dir().join("oxide-prompt-syntax-test");
        std::fs::create_dir_all(&dir).unwrap();
        let zsh_path = dir.join("init.zsh");
        let bash_path = dir.join("init.bash");
        std::fs::write(&zsh_path, generate_init(&PromptConfig::default(), true)).unwrap();
        std::fs::write(
            &bash_path,
            generate_init_bash(&PromptConfig::default(), true),
        )
        .unwrap();
        for (shell, path) in [("/bin/zsh", &zsh_path), ("/bin/bash", &bash_path)] {
            if !std::path::Path::new(shell).exists() {
                continue;
            }
            let status = std::process::Command::new(shell)
                .arg("-n")
                .arg(path)
                .status()
                .unwrap();
            assert!(status.success(), "{shell} rejected generated script");
        }
    }

    /// M8, headless: a real zsh under the ZDOTDIR shim renders the segmented
    /// prompt (with powerline separators) into the terminal grid.
    #[test]
    fn zsh_prompt_end_to_end() {
        use crate::terminal::session::{SessionOptions, TermSize, TerminalSession};
        use std::time::{Duration, Instant};

        if !std::path::Path::new("/bin/zsh").exists() {
            return;
        }
        let config = crate::config::Config::default();
        let integration = crate::prompt::integration::setup(&config, "/bin/zsh");
        let Some(zdotdir) = integration.env.get("ZDOTDIR").cloned() else {
            return; // cache dir unavailable in this environment
        };
        assert!(std::path::Path::new(&zdotdir).join(".zshrc").exists());

        let size = TermSize {
            columns: 120,
            screen_lines: 24,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let options = SessionOptions {
            program: "/bin/zsh".into(),
            args: vec![],
            working_directory: std::env::current_dir().ok(),
            scrollback: 100,
            env: integration.env,
        };
        let (session, _rx) = TerminalSession::spawn(options, size).expect("spawn zsh");

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            std::thread::sleep(Duration::from_millis(150));
            let text: String = {
                let term = session.term.lock();
                term.renderable_content()
                    .display_iter
                    .map(|i| i.cell.c)
                    .collect()
            };
            // Default segments end with the powerline arrow; cwd segment shows
            // the repo basename.
            if text.contains('\u{e0b0}') && text.contains("oxide") {
                break;
            }
            if Instant::now() > deadline {
                panic!("prompt never rendered; grid: {text}");
            }
        }
    }

    #[test]
    fn env_segment_rejects_injection() {
        use crate::config::schema::{SegmentConfig, SegmentKind, SegmentOptions};
        let seg = SegmentConfig {
            kind: SegmentKind::Env,
            options: SegmentOptions {
                var: Some("$(rm -rf /)".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(segment_snippet(0, &seg), "");
    }
}

#[cfg(test)]
mod cd_tests {
    use crate::config::Config;
    use crate::terminal::session::{SessionOptions, TermSize, TerminalSession};
    use std::time::{Duration, Instant};

    fn grid(session: &TerminalSession) -> String {
        let term = session.term.lock();
        let mut out = String::new();
        let mut line = i32::MIN;
        for ix in term.renderable_content().display_iter {
            if ix.point.line.0 != line {
                out.push('\n');
                line = ix.point.line.0;
            }
            out.push(ix.cell.c);
        }
        out
    }

    /// The tree's `c` must change the shell's directory with no `cd` echoed.
    /// Runs against every bash on the machine — Apple's /bin/bash 3.2 has a
    /// broken multi-char `bind -x` that needs the trampoline path, while a
    /// Homebrew bash 5 exercises the direct binding.
    #[test]
    fn silent_cd_in_bash() {
        for bash in ["/opt/homebrew/bin/bash", "/bin/bash"] {
            if std::path::Path::new(bash).exists() {
                silent_cd_scenario(bash);
            }
        }
    }

    fn silent_cd_scenario(bash: &str) {
        let config = Config::default();
        let integration = crate::prompt::integration::setup(&config, bash);
        let Some(args) = integration.args_override.clone() else {
            return;
        };

        let size = TermSize {
            columns: 100,
            screen_lines: 24,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let options = SessionOptions {
            program: bash.to_string(),
            args,
            working_directory: Some("/Users/bobby/Developer/oxide".into()),
            scrollback: 100,
            env: integration.env.clone(),
        };
        let (session, _rx) = TerminalSession::spawn(options, size).expect("spawn bash");
        std::thread::sleep(Duration::from_millis(1500)); // let rc files load

        let target = crate::prompt::integration::cd_target_path().expect("cd target path");
        std::fs::write(&target, "/usr/local").unwrap();
        let before = grid(&session);
        // Mirrors TerminalPane::request_cd for bash: trigger + empty line.
        session.write_input(b"\x1b[9001~\r".to_vec());

        // Wait for the shell to move *and* for a refreshed prompt to render.
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            std::thread::sleep(Duration::from_millis(200));
            let moved = session
                .foreground_cwd()
                .is_some_and(|p| p == std::path::Path::new("/usr/local"));
            let last_line = grid(&session)
                .lines()
                .filter(|l| !l.trim().is_empty())
                .next_back()
                .unwrap_or("")
                .to_string();
            if moved && last_line.contains("local") {
                break;
            }
            if Instant::now() > deadline {
                panic!(
                    "moved={moved}, prompt never refreshed; last line: {last_line:?}\ngrid:\n{}",
                    grid(&session)
                );
            }
        }
        let after = grid(&session);
        let new_text = after.replace(&before, "");
        assert!(
            !new_text.contains("cd /usr/local") && !new_text.contains("cd '"),
            "a cd command was echoed:\n{new_text}"
        );
        // The visible prompt must reflect the new directory immediately,
        // without the user having to run a command first.
        let last = after
            .lines()
            .filter(|l| !l.trim().is_empty())
            .next_back()
            .unwrap_or("");
        assert!(
            last.contains("local"),
            "prompt did not refresh to the new directory; last line: {last:?}\nfull:\n{after}"
        );
        // The prompt we left behind must be erased, not stacked above the new
        // one — only the current directory should be on screen.
        assert!(
            !after.contains("oxide"),
            "stale prompt left on screen after cd:\n{after}"
        );
    }
}
