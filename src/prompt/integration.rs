use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::config::schema::PromptConfig;

/// File the app writes a directory into for the shell's silent-cd widget.
pub fn cd_target_path() -> Option<PathBuf> {
    Some(cache_dir()?.join("cd_target"))
}

fn cache_dir() -> Option<PathBuf> {
    let home = directories::BaseDirs::new()?.home_dir().to_path_buf();
    Some(home.join(".cache/oxide"))
}

#[derive(Default, Clone)]
pub struct ShellIntegration {
    pub env: HashMap<String, String>,
    /// Replacement argv for the shell, when injection needs different flags.
    pub args_override: Option<Vec<String>>,
}

/// Write the generated init scripts and return env/args for the child shell.
/// Never touches the user's real dotfiles.
///
/// zsh: a ZDOTDIR shim whose rc files each source the user's counterpart with
/// ZDOTDIR temporarily restored, then layer our init.zsh on top (so our precmd
/// hook — and therefore PROMPT — wins over anything the user's config set).
///
/// bash: `--init-file init.bash` (replacing `-l`, which would make bash skip
/// the init file); the script emulates the login profile chain first.
pub fn setup(config: &Config, shell_program: &str) -> ShellIntegration {
    let mut integration = ShellIntegration::default();
    let style_prompt = config.prompt.enabled;
    if !config.shell.integration && !style_prompt {
        return integration;
    }
    let shell_name = Path::new(shell_program)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let Some(cache) = cache_dir() else { return integration };
    if std::fs::create_dir_all(&cache).is_err() {
        return integration;
    }

    if shell_name.starts_with("zsh") {
        let zdotdir = cache.join("zdotdir");
        if std::fs::create_dir_all(&zdotdir).is_err() {
            return integration;
        }
        if write_zsh_shim(&cache, &zdotdir, &config.prompt, style_prompt).is_err() {
            return integration;
        }
        if let Ok(user_zdotdir) = std::env::var("ZDOTDIR") {
            integration.env.insert("_OXIDE_USER_ZDOTDIR".into(), user_zdotdir);
        }
        integration
            .env
            .insert("ZDOTDIR".into(), zdotdir.to_string_lossy().to_string());
    } else if shell_name.starts_with("bash") {
        let init_path = cache.join("init.bash");
        if std::fs::write(&init_path, super::generate_init_bash(&config.prompt, style_prompt)).is_err() {
            return integration;
        }
        let mut args: Vec<String> = config
            .shell
            .args
            .iter()
            .filter(|a| a.as_str() != "-l" && a.as_str() != "--login")
            .cloned()
            .collect();
        args.push("--init-file".into());
        args.push(init_path.to_string_lossy().to_string());
        integration.args_override = Some(args);
    }
    integration
}

fn write_zsh_shim(
    cache: &Path,
    zdotdir: &Path,
    prompt: &PromptConfig,
    style_prompt: bool,
) -> std::io::Result<()> {
    let init_path = cache.join("init.zsh");
    std::fs::write(&init_path, super::generate_init(prompt, style_prompt))?;

    let sandwich = |file: &str, extra: &str| -> String {
        format!(
            r#"# Oxide ZDOTDIR shim — sources your real {file}, never modifies it.
_oxide_shim="$ZDOTDIR"
export ZDOTDIR="${{_OXIDE_USER_ZDOTDIR:-$HOME}}"
[[ -f "$ZDOTDIR/{file}" ]] && builtin source "$ZDOTDIR/{file}"
export _OXIDE_USER_ZDOTDIR="$ZDOTDIR"
export ZDOTDIR="$_oxide_shim"
unset _oxide_shim
{extra}"#
        )
    };

    std::fs::write(zdotdir.join(".zshenv"), sandwich(".zshenv", ""))?;
    std::fs::write(zdotdir.join(".zprofile"), sandwich(".zprofile", ""))?;
    let zshrc_tail = format!(
        "builtin source \"{}\"\n# Hand rc-file resolution back to the user's zsh for subshells.\nexport ZDOTDIR=\"${{_OXIDE_USER_ZDOTDIR:-$HOME}}\"\nunset _OXIDE_USER_ZDOTDIR\n",
        init_path.to_string_lossy()
    );
    std::fs::write(zdotdir.join(".zshrc"), sandwich(".zshrc", &zshrc_tail))?;
    Ok(())
}
