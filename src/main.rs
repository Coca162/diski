type ExResult<T> = Result<T, Box<dyn std::error::Error + 'static>>;

use std::{collections::HashMap, env::args, future::Future, ops::Not};

use futures::StreamExt;
use ksni::TrayMethods;
use notify_rust::Notification;
use tokio::{select, sync::mpsc, try_join};
use zbus::zvariant::OwnedObjectPath;
use zbus_polkit::policykit1::{AuthorityProxy, CheckAuthorizationFlags, Subject};
use zbus_systemd::systemd1::{ManagerProxy, UnitProxy};

#[derive(Debug)]
struct DiskTray {
    display_name: String,
    mount: State,
    automount: State,
    requester: mpsc::UnboundedSender<ClientRequests>,
}

#[derive(Debug)]
enum ClientRequests {
    PrepareDisconnect,
    EnableAutomounting,
}

impl ksni::Tray for DiskTray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        env!("CARGO_PKG_NAME").into()
    }
    fn icon_name(&self) -> String {
        "drive-harddisk".into()
    }
    fn title(&self) -> String {
        format!("{} Status", self.display_name)
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        vec![
            StandardItem {
                label: format!("Mount: {:?}", self.mount),
                enabled: false,
                disposition: Disposition::Informative,
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: format!("Automount: {:?}", self.automount),
                enabled: false,
                disposition: Disposition::Informative,
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Disconnect".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.requester.send(ClientRequests::PrepareDisconnect);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Enable automount".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.requester.send(ClientRequests::EnableAutomounting);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExResult<()> {
    let mut args = args().skip(1);
    let systemd_name = args.next().expect("Expected drive name!");

    let display_name = args.next().expect("Expected name!");

    let conn = zbus::Connection::system().await?;

    let mount_name = format!("{systemd_name}.mount");
    let automount_name = format!("{systemd_name}.automount");

    let authority = AuthorityProxy::new(&conn).await?;
    let subject = Subject::new_for_owner(std::process::id(), None, None)?;

    let manager = zbus_systemd::systemd1::ManagerProxy::new(&conn).await?;
    let mount = manager.get_unit(mount_name.clone()).await?;
    let automount = manager.get_unit(automount_name.clone()).await?;

    let mount = UnitProxy::new(&conn, mount).await?;
    let automount = UnitProxy::new(&conn, automount).await?;

    let mut mount_state = State::from_substates(&mount.sub_state().await?);
    let mut automount_state = State::from_substates(&automount.sub_state().await?);

    let (sender, mut events) = mpsc::unbounded_channel();

    let tray = DiskTray {
        display_name,
        mount: mount_state,
        automount: automount_state,
        requester: sender,
    };

    let handle = tray.spawn().await.unwrap();

    let mut mount_state_change = mount.receive_sub_state_changed().await;
    let mut automount_state_change = automount.receive_sub_state_changed().await;

    manager.subscribe().await?;

    loop {
        select! {
            biased;
            s = mount_state_change.next()  => {
                let new = State::from_substates(&s.unwrap().get().await.unwrap());

                if new != mount_state {
                    mount_state = new;
                    handle.update(|t| t.mount = dbg!(mount_state)).await;
                }
            }
            s = automount_state_change.next() => {
                let new = State::from_substates(&s.unwrap().get().await.unwrap());

                if new != automount_state {
                    automount_state = new;
                    handle.update(|t| t.automount = dbg!(automount_state)).await;
                }
            }
            Some(req) = events.recv() => {
                let result = authority
                    .check_authorization(
                        &subject,
                        "org.freedesktop.systemd1.manage-units",
                        &HashMap::default(),
                        CheckAuthorizationFlags::AllowUserInteraction.into(),
                        "",
                    )
                    .await?;

                if result.is_authorized.not() {
                    continue;
                }

                match req {
                    ClientRequests::PrepareDisconnect => {
                        try_join!(
                            job_wait(&manager, automount.stop("replace".into())),
                            job_wait(&manager, mount.stop("replace".into()))
                        )?;

                        Notification::new()
                            .summary(&systemd_name)
                            .body("Drive has been fully unmounted")
                            .icon("drive-harddisk")
                            .show_async().await?;
                    },
                    ClientRequests::EnableAutomounting => {
                        job_wait(&manager, automount.start("replace".into())).await?;

                        Notification::new()
                            .summary(&systemd_name)
                            .body("Automounting has been enabled")
                            .icon("drive-harddisk")
                            .show_async().await?;
                    },
                }
            }
        }
    }
}

async fn job_wait(
    manager: &ManagerProxy<'_>,
    job_future: impl Future<Output = zbus::Result<OwnedObjectPath>>,
) -> ExResult<()> {
    let mut removed_stream = manager.receive_job_removed().await?;

    let job = job_future.await?;

    loop {
        let removed = removed_stream.next().await.unwrap();
        if removed.args()?.job == job {
            return Ok(());
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Mounted,
    Mounting,
    Unmounting,
    Dead,
    Waiting,
    Running,
    Failed,
}

impl State {
    fn from_substates(input: &str) -> Self {
        match input {
            "mounted" | "mounting-done" => Self::Mounted,
            "mounting" => Self::Mounting,
            "unmounting" => Self::Unmounting,
            "dead" => Self::Dead,
            "waiting" => Self::Waiting,
            "running" => Self::Running,
            "failed" => Self::Failed,
            input => panic!("Unexpected active state: {input}"),
        }
    }
}
