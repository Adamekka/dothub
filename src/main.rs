use exec;
use fork::{daemon, Fork};
use std::{
    collections::HashMap,
    env, fs,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
    process,
    time::Duration,
};

use clap::{Arg, ArgMatches, Command};
use notify::{Config, PollWatcher, Watcher};
use serde_derive::Deserialize;

macro_rules! get_dot_or_df_opt {
    ($dot_conf:ident,$df_conf:ident,$var:ident) => {
        if let Some(x) = &$dot_conf.$var {
            Some(x.clone())
        } else {
            $df_conf.$var.clone()
        }
    };
}

#[derive(Debug)]
struct DotProfile {
    name: String,
    start: Option<Vec<String>>,
    dots: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct DotProfileParsable {
    start: Option<Vec<String>>,
    dots: HashMap<String, String>,
}

#[derive(Debug)]
struct DotFolder {
    name: String,
    dots: Vec<Dot>,
    config: Option<DotConfig>,
}

#[derive(Debug)]
struct Dot {
    name: String,
    config: Option<DotConfig>,
}

#[derive(Debug, Deserialize, Clone)]
struct DotConfig {
    start: Option<String>,
    kill: Option<String>,
    reload: Option<String>,
    destination: String,
    reload_on_set: Option<bool>,
}

fn main() {
    // check if $HOME/.dothub exists, if not, create one
    let user_home = env::var("HOME").expect("No $HOME set!");
    let folder_path = user_home.clone() + "/.dothub";
    let folder_path = Path::new(&folder_path);

    if !folder_path.exists() {
        fs::create_dir(folder_path).expect("Couldn't create '.dothub' in your $HOME");
    }

    let profiles_path = &folder_path.join("profiles");

    if !profiles_path.exists() {
        fs::create_dir(profiles_path).expect("Couldn't create 'profiles' in your .dothub .");
    }

    // go through .dothub/profiles and initialize all DotProfiles
    let mut profiles: Vec<DotProfile> = vec![];

    for profile_file in fs::read_dir(profiles_path).unwrap() {
        let profile_file = profile_file.expect("Couldn't read DotProfile").path();

        if profile_file.is_file() {
            profiles.push(process_dotprofile(profile_file));
        }
    }

    // go through .dothub and initialize all DotFolders with their Dots
    let mut dot_folders: Vec<DotFolder> = vec![];

    for dot_folder in fs::read_dir(folder_path).unwrap() {
        let dot_folder = dot_folder.expect("Couldn't read DotFolder.").path();

        if dot_folder.is_dir() && !dot_folder.ends_with("profiles") {
            dot_folders.push(process_dotfolder(&dot_folder));
        }
    }

    // helper functions
    let get_dot_info_from_args = |args: &ArgMatches| -> (&DotFolder, Option<&Dot>) {
        let dotfolder_arg = args.get_one::<String>("DotFolder").unwrap();
        let dot_arg = args.get_one::<String>("Dot");

        let dotfolder = match dot_folders.iter().find(|df| &df.name == dotfolder_arg) {
            Some(df) => df,
            None => panic!("No DotFolder named '{}'", &dotfolder_arg),
        };

        if let Some(dot_arg) = dot_arg {
            let dot = match dotfolder.dots.iter().find(|d| &d.name == dot_arg) {
                Some(d) => d,
                None => panic!("No Dot named '{}'", &dot_arg),
            };

            return (dotfolder, Some(dot));
        }

        (dotfolder, None)
    };

    let get_active_config = |dot_info: (&DotFolder, Option<&Dot>)| -> DotConfig {
        let (dotfolder, dot) = dot_info;

        if dotfolder.config.is_none() {
            panic!("DotFolder has to have a .dothub with at least 'destination' filled!");
        }

        // for the love of god, rewrite this
        // also two structs dotFolderConfig, dotConfig, dumbass
        if let Some(dot) = dot {
            if let Some(config) = &dot.config {
                // merge
                let df_config = dotfolder.config.as_ref().unwrap();

                DotConfig {
                    start: { get_dot_or_df_opt!(config, df_config, start) },
                    kill: { get_dot_or_df_opt!(config, df_config, kill) },
                    reload: { get_dot_or_df_opt!(config, df_config, reload) },
                    destination: {
                        if config.destination.is_empty() {
                            df_config.destination.clone()
                        } else {
                            config.destination.clone()
                        }
                    },
                    reload_on_set: { get_dot_or_df_opt!(config, df_config, reload_on_set) },
                }
            } else {
                dotfolder.config.as_ref().expect("yes").clone()
            }
        } else {
            dotfolder.config.as_ref().expect("yes").clone()
        }
    };

    // commands
    let args = arguments();

    match args.subcommand() {
        Some(("set", set_matches)) => {
            let (dotfolder, dot) = get_dot_info_from_args(&set_matches);
            let config = get_active_config((dotfolder, dot));
            let dot = dot.unwrap();

            let conf_path = Path::new(&config.destination);
            let dot_path = format!(
                "{}/{}/{}",
                folder_path.to_str().unwrap(),
                dotfolder.name,
                dot.name
            );
            let dot_path = Path::new(&dot_path);

            dot_set(&config, &dot_path, &conf_path);
        }
        Some(("watch", set_matches)) => {
            let (dotfolder, dot) = get_dot_info_from_args(&set_matches);
            let config = get_active_config((dotfolder, dot));
            let dot = dot.unwrap();

            if let Some(_) = &dotfolder.config {
                let conf_path = Path::new(&config.destination);
                let dot_path = format!(
                    "{}/{}/{}",
                    folder_path.to_str().unwrap(),
                    dotfolder.name,
                    dot.name
                );
                let dot_path = Path::new(&dot_path);

                dot_set(&config, &dot_path, &conf_path);

                // watch for directory changes (writes, moves, etc..)
                let (tx, rx) = std::sync::mpsc::channel();

                let mut watcher = PollWatcher::new(
                    tx,
                    Config::default().with_poll_interval(Duration::from_secs(1)),
                )
                .expect("Couldn't create watcher");

                watcher
                    .watch(&dot_path, notify::RecursiveMode::Recursive)
                    .expect("Couldn't add Dot path to watcher.");

                for res in rx {
                    match res {
                        Ok(ev) => {
                            if ev.paths[0].is_file() {
                                dot_reload(&config.clone());
                            }
                        }
                        Err(e) => println!("watch error: {:?}", e),
                    }
                }
            } else {
                panic!("DotFolder has to have a .dothub with at least 'destination' filled!")
            }
        }
        Some(("list", _)) => {
            for df in dot_folders {
                println!("{}/", df.name);
                for d in df.dots {
                    println!("\t{}", d.name);
                }
            }
        }
        Some(("start", matches)) => {
            let config = get_active_config(get_dot_info_from_args(&matches));

            dot_start(&config);
        }
        Some(("kill", matches)) => {
            let config = get_active_config(get_dot_info_from_args(&matches));

            dot_kill(&config);
        }
        Some(("reload", matches)) => {
            let config = get_active_config(get_dot_info_from_args(&matches));

            dot_reload(&config);
        }
        Some(("edit", _)) => {
            let editor = env::var("EDITOR").expect("$EDITOR has to be set!");

            run(&editor);
        }
        Some(("run", matches)) => {
            let prog = matches.get_one("Program").unwrap();

            run(prog);
        }
        Some(("profile", matches)) => match matches.subcommand() {
            Some(("set", pmatches)) => {
                let to_set = pmatches.get_one::<String>("DotProfile").unwrap();

                let profile = profiles
                    .iter()
                    .find(|dp| &dp.name == to_set)
                    .expect("DotProfile doesn't exist!");

                // run profile on_start commands
                if let Some(start) = &profile.start {
                    for cmd in start {
                        process::Command::new("/usr/bin/bash")
                            .args(["-c", &cmd])
                            .output()
                            .expect("Couldn't run command '{cmd}'");
                    }
                }

                // set all dots from dotprofile
                for (df, dt) in profile.dots.iter() {
                    let dotfolder_path = folder_path.join(df);
                    let dot_path = dotfolder_path.join(dt);

                    let dotfolder = process_dotfolder(&dotfolder_path);
                    let dot = process_dot(&dot_path);
                    let config = get_active_config((&dotfolder, Some(&dot)));
                    let conf_path = Path::new(&config.destination);

                    dot_set(&config, &dot_path, &conf_path);
                }
            }
            Some(("list", _)) => {
                for dp in profiles {
                    println!("{}", dp.name);
                }
            }
            _ => unreachable!(),
        },
        _ => unreachable!(),
    }
}

fn dot_set(config: &DotConfig, dot_path: &Path, conf_path: &Path) {
    if !conf_path.exists() {
        if let Some(parent_path) = conf_path.parent() {
            if !parent_path.exists() {
                fs::create_dir_all(parent_path).expect("Couldn't create parent path recursively.");
            }
        }
    }

    if conf_path.is_file() {
        fs::remove_file(conf_path).expect("Coudln't remove old dot file.");
    } else if conf_path.is_dir() {
        fs::remove_dir_all(conf_path).expect("Couldn't remove the old Dot folder.");
    }

    symlink(dot_path, conf_path).expect("Couldn't create a symlink.");

    if config.reload.is_some() || (config.start.is_some() && config.kill.is_some()) {
        match config.reload_on_set {
            Some(x) if x == true => dot_reload(&config),
            None => dot_reload(&config),
            _ => {}
        }
    }
}
// run a program, make it a daemon, exit
fn run(prog: &String) {
    if let Ok(Fork::Child) = daemon(false, false) {
        let _ = exec::Command::new("sh").args(&["-c", prog]).exec();
    }
}
fn dot_start(config: &DotConfig) {
    if let Some(start_cmd) = &config.start {
        run(start_cmd);
    }

    // will keep this here for now..

    //     match unsafe { fork() } {
    //         Ok(ForkResult::Parent { child: _ }) => {
    //             exit(0);
    //         }
    //         Ok(ForkResult::Child) => {
    //             setsid().unwrap();
    //             exec::Command::new("sh").args(&["-c", start_cmd]).exec();
    //         }
    //         Err(e) => panic!("Couldn't fork."),
    //     }
    // } else {
    //     panic!("No 'start' command specified in any .dothub .");
    // }
}
fn dot_kill(config: &DotConfig) {
    if let Some(kill_cmd) = &config.kill {
        process::Command::new("sh")
            .args(["-c", kill_cmd])
            .output()
            .expect("Couldn't kill Dot");
    } else {
        panic!("No 'kill' command specified in any .dothub .");
    }
}
fn dot_reload(config: &DotConfig) {
    if let Some(reload_cmd) = &config.reload {
        process::Command::new("sh")
            .args(["-c", &reload_cmd])
            .output()
            .expect("Couldn't reload Dot");
    } else if let (Some(start_cmd), Some(kill_cmd)) = (&config.start, &config.kill) {
        process::Command::new("sh")
            .args(["-c", &format!("{} && {}", &kill_cmd, &start_cmd)])
            .output()
            .expect("Couldn't reload Dot");
    } else {
        panic!("No 'reload' command specified in any .dothub .");
    }
}

fn process_dotprofile(path: PathBuf) -> DotProfile {
    let name = path.file_name().unwrap().to_str().unwrap().to_owned();

    let profile_contents = fs::read_to_string(&path).expect("Couldn't read DotProfile.");

    let parsed: DotProfileParsable =
        toml::from_str(&profile_contents).expect("Couldn't parse a DotProfile.");

    DotProfile {
        name,
        start: parsed.start,
        dots: parsed.dots,
    }
}

fn process_dotfolder(path: &PathBuf) -> DotFolder {
    let name = path.file_name().unwrap().to_str().unwrap().to_owned();
    let mut config: Option<DotConfig> = None;

    if !path.exists() {
        panic!("DotFolder '{}' doesn't exist!", name);
    }

    let dots_paths = path.read_dir().unwrap();
    let dots: Vec<Dot> = dots_paths
        .filter_map(|dot_path| {
            let dot_path = dot_path.expect("Couldn't read Dot.").path();
            let dot_path_name = dot_path.file_name().unwrap().to_str().unwrap();

            if dot_path.is_dir() {
                return Some(process_dot(&dot_path));
            } else if dot_path.is_file() && dot_path_name == ".dothub" {
                let config_file = fs::read_to_string(dot_path)
                    .expect("Couldn't read .dothub .")
                    .replace("~", &env::var("HOME").expect("No $HOME set!"));

                config = Some(toml::from_str(&config_file).expect("Couldn't parse .dothub ."));
            }
            None
        })
        .collect();

    DotFolder { name, dots, config }
}

fn process_dot(path: &PathBuf) -> Dot {
    let name = path.file_name().unwrap().to_str().unwrap().to_owned();
    let mut config: Option<DotConfig> = None;

    if !path.exists() {
        panic!("Dot '{}' doesn't exist!", name);
    }

    let dots_files = path.read_dir().unwrap();
    for dot_path in dots_files {
        let dot_path = dot_path.expect("Couldn't read Dot.").path();
        let dot_path_name = dot_path.file_name().unwrap().to_str().unwrap();

        if dot_path.is_file() && dot_path_name == ".dothub" {
            let config_file = fs::read_to_string(dot_path)
                .expect("Couldn't read .dothub .")
                .replace("~", &env::var("HOME").expect("No $HOME set!"));

            config = Some(toml::from_str(&config_file).expect("Couldn't parse .dothub ."));
            break;
        }
    }

    Dot { name, config }
}

fn arguments() -> clap::ArgMatches {
    Command::new("dothub")
        .about("Manage your dofiles from a comfortable hub!")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .author("Yours truly")
        .subcommand(
            Command::new("set")
                .about("Applies a Dot.")
                .arg(Arg::new("Dot location")
                    .help("Dotfolder/Dot, example 'waybar/neon'.")
                    .required(true))
        )
        .subcommand(
            Command::new("watch")
                .about("Watches a Dot and reloads on a change.")
                .arg(Arg::new("Dot location")
                    .help("Dotfolder/Dot, example 'waybar/neon'.")
                    .required(true))
        )
        .subcommand(
            Command::new("list")
                .about("Lists all the avaiable Dots.")   
        )
        .subcommand(
            Command::new("start")
                .about("Starts a Dot, DotFolder config used if Dot isn't specified, or there is no Dot config")
                .arg(Arg::new("DotFolder").required(true))
                .arg(Arg::new("Dot"))
        )
        .subcommand(
            Command::new("kill")
                .about("Kills a Dot, DotFolder config used if Dot isn't specified, or there is no Dot config.")
                .arg(Arg::new("DotFolder").required(true))
                .arg(Arg::new("Dot"))
        )
        .subcommand(
            Command::new("reload")
                .about("Reloads a Dot, DotFolder config used if Dot isn't specified, or there is no Dot config.")
                .arg(Arg::new("DotFolder").required(true))
                .arg(Arg::new("Dot"))
        )
        .subcommand(
            Command::new("run")
                .about("Runs a program as a daemon, aka, doesn't halt output.")
                .arg(Arg::new("Program").required(true))
        )
        .subcommand(
            Command::new("edit")
                .about("Edits a Dot with your $EDITOR.")
                .arg(Arg::new("DotFolder").required(true))
                .arg(Arg::new("Dot"))
        )
        // .subcommand(
        //     Command::new("get")
        //         .about("'Gets' a local/git folder.")
        // )
        .subcommand(
            Command::new("profile")
                .about("Profiles")
                .subcommand_required(true)
                .arg_required_else_help(true)
                .subcommand(
                    Command::new("set")
                        .about("Sets a profile.")
                        .arg(Arg::new("DotProfile").required(true))
                )
                .subcommand(
                    Command::new("list")
                )
                // .subcommand(
                //     Command::new("create_from_current")
                // )
        )
        .get_matches()
}
