use std::fs;
use std::path::{PathBuf, Path};
use std::process::Command;

use fn_error_context::context;

use crate::process::{run, exit_from};
use crate::server::options::{Start, Stop, Restart, Status};
use crate::server::init::{data_path, Metadata};
use crate::server::methods::InstallMethod;
use crate::server::version::Version;
use crate::server::{linux, macos};
use crate::server::status;
use crate::platform::{home_dir, get_current_uid};


pub trait Instance {
    fn start(&mut self, options: &Start) -> anyhow::Result<()>;
    fn stop(&mut self, options: &Stop) -> anyhow::Result<()>;
    fn restart(&mut self, options: &Restart) -> anyhow::Result<()>;
    fn status(&mut self, options: &Status) -> anyhow::Result<()>;
    fn get_status(&self) -> anyhow::Result<status::Status>;
    fn get_socket(&self, admin: bool) -> anyhow::Result<PathBuf>;
    fn run_command(&self) -> anyhow::Result<Command>;
}

pub struct SystemdInstance {
    name: String,
    #[allow(dead_code)]
    system: bool,
    version: Version<String>,
    data_dir: PathBuf,
    port: u16,
}

pub struct LaunchdInstance {
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    system: bool,
    #[allow(dead_code)]
    version: Version<String>,
    unit_path: PathBuf,
    data_dir: PathBuf,
    port: u16,
}

#[context("failed to read metadata {}/metadata.json", dir.display())]
pub fn read_metadata(dir: &Path) -> anyhow::Result<Metadata> {
    let metadata_path = dir.join("metadata.json");
    Ok(serde_json::from_slice(&fs::read(&metadata_path)?)?)
}

pub fn get_instance(name: &str) -> anyhow::Result<Box<dyn Instance>> {
    let dir = data_path(false)?.join(name);
    let system = if dir.exists() {
        false
    } else {
        /*  // TODO(tailhook) implement system instances
        let sys_dir = data_path(true)?.join(name);
        if sys_dir.exists() {
            anyhow::bail!("System instances are not implemented yet");
        }
        */
        anyhow::bail!("No instance {0:?} found. Run:\n  \
            edgedb server init {0}", name);
    };
    log::debug!("Instance {:?} data path: {:?}", name, dir);
    let metadata = read_metadata(&dir)?;
    get_instance_from_metadata(name, system, &metadata)
}

pub fn get_instance_from_metadata(name: &str, system: bool,
    metadata: &Metadata)
 -> anyhow::Result<Box<dyn Instance>>
{
    let dir = data_path(false)?.join(name);
    match metadata.method {
        InstallMethod::Package if cfg!(target_os="linux") => {
            Ok(Box::new(SystemdInstance {
                name: name.to_owned(),
                system,
                version: metadata.version.to_owned(),
                port: metadata.port,
                data_dir: dir,
            }))
        }
        InstallMethod::Package if cfg!(target_os="macos") => {
            let unit_name = format!("com.edgedb.edgedb-server-{}.plist", name);
            Ok(Box::new(LaunchdInstance {
                name: name.to_owned(),
                system,
                version: metadata.version.to_owned(),
                data_dir: dir,
                unit_path: home_dir()?.join("Library/LaunchAgents")
                    .join(&unit_name),
                port: metadata.port,
            }))
        }
        _ => {
            anyhow::bail!("Unknown installation method and OS combination");
        }
    }
}

impl Instance for SystemdInstance {
    fn start(&mut self, options: &Start) -> anyhow::Result<()> {
        if options.foreground {
            run(&mut self.run_command()?)?;
        } else {
            run(Command::new("systemctl")
                .arg("--user")
                .arg("start")
                .arg(format!("edgedb-server@{}", self.name)))?;
        }
        Ok(())
    }
    fn stop(&mut self, _options: &Stop) -> anyhow::Result<()> {
        run(Command::new("systemctl")
            .arg("--user")
            .arg("stop")
            .arg(format!("edgedb-server@{}", self.name)))?;
        Ok(())
    }
    fn restart(&mut self, _options: &Restart) -> anyhow::Result<()> {
        run(Command::new("systemctl")
            .arg("--user")
            .arg("restart")
            .arg(format!("edgedb-server@{}", self.name)))?;
        Ok(())
    }
    fn status(&mut self, options: &Status) -> anyhow::Result<()> {
        if options.extended {
            self.get_status()?.print_extended_and_exit();
        } else if options.service {
            exit_from(Command::new("systemctl")
                .arg("--user")
                .arg("status")
                .arg(format!("edgedb-server@{}", self.name)))?;
        } else {
            self.get_status()?.print_and_exit();
        }
        Ok(())
    }
    fn get_status(&self) -> anyhow::Result<status::Status> {
        status::get_status(&self.name, self.system)
    }
    fn get_socket(&self, admin: bool) -> anyhow::Result<PathBuf> {
        Ok(dirs::runtime_dir()
            .unwrap_or_else(|| {
                Path::new("/run/user").join(get_current_uid().to_string())
            })
            .join(format!("edgedb-{}", self.name))
            .join(format!(".s.EDGEDB{}.{}",
                if admin { ".admin" } else { "" },
                self.port)))
    }
    fn run_command(&self) -> anyhow::Result<Command> {
        let sock = self.get_socket(true)?;
        let socket_dir = sock.parent().unwrap();
        let mut cmd = Command::new(linux::get_server_path(&self.version));
        cmd.arg("--port").arg(self.port.to_string());
        cmd.arg("--data-dir").arg(&self.data_dir);
        cmd.arg("--runstate-dir").arg(&socket_dir);
        Ok(cmd)
    }
}

impl LaunchdInstance {
    fn launchd_name(&self) -> String {
        format!("gui/{}/edgedb-server-{}", get_current_uid(), self.name)
    }
}

impl Instance for LaunchdInstance {
    fn start(&mut self, options: &Start) -> anyhow::Result<()> {
        if options.foreground {
            run(&mut self.run_command()?)?;
        } else {
            run(Command::new("launchctl")
                .arg("load").arg("-w")
                .arg(&self.unit_path))?;
        }
        Ok(())
    }
    fn stop(&mut self, _options: &Stop) -> anyhow::Result<()> {
        run(Command::new("launchctl")
            .arg("unload")
            .arg(&self.unit_path))?;
        Ok(())
    }
    fn restart(&mut self, _options: &Restart) -> anyhow::Result<()> {
        run(Command::new("launchctl")
            .arg("kickstart")
            .arg("-k")
            .arg(self.launchd_name()))?;
        Ok(())
    }
    fn status(&mut self, options: &Status) -> anyhow::Result<()> {
        if options.extended {
            self.get_status()?.print_extended_and_exit();
        } else if options.service {
            exit_from(Command::new("launchctl")
                .arg("print")
                .arg(self.launchd_name()))?;
        } else {
            self.get_status()?.print_and_exit();
        }
        Ok(())
    }
    fn get_status(&self) -> anyhow::Result<status::Status> {
        status::get_status(&self.name, self.system)
    }
    fn get_socket(&self, admin: bool) -> anyhow::Result<PathBuf> {
        Ok(home_dir()?
            .join(".edgedb/run")
            .join(&self.name)
            .join(format!(".s.EDGEDB{}.{}",
                if admin { ".admin" } else { "" },
                self.port)))
    }
    fn run_command(&self) -> anyhow::Result<Command> {
        let sock = self.get_socket(true)?;
        let socket_dir = sock.parent().unwrap();
        let mut cmd = Command::new(macos::get_server_path(&self.version)?);
        cmd.arg("--port").arg(self.port.to_string());
        cmd.arg("--data-dir").arg(&self.data_dir);
        cmd.arg("--runstate-dir").arg(&socket_dir);
        Ok(cmd)
    }
}
