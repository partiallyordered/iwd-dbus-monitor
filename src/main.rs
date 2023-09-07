use std::collections::HashMap;

use zbus::{Connection, fdo};

#[derive(Debug)]
enum Error {
    ZBus(zbus::Error),
    ZVariant(zbus::zvariant::Error),
    ZBusFdo(zbus::fdo::Error),
    Serde(serde_json::Error),
}

impl From<zbus::fdo::Error> for Error {
    fn from(value: zbus::fdo::Error) -> Self {
        Self::ZBusFdo(value)
    }
}

impl From<zbus::zvariant::Error> for Error {
    fn from(value: zbus::zvariant::Error) -> Self {
        Self::ZVariant(value)
    }
}

impl From<zbus::Error> for Error {
    fn from(value: zbus::Error) -> Self {
        Self::ZBus(value)
    }
}

impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self::Serde(value)
    }
}

#[async_std::main]
async fn main() -> std::result::Result<(), Error> {
    let connection = Connection::system().await?;

    let object_manager_proxy = fdo::ObjectManagerProxy::builder(&connection)
        .destination("net.connman.iwd")?
        .path("/")?
        .build()
        .await?;

    let managed_objects = object_manager_proxy.get_managed_objects().await?;

    let network_interface_objects = managed_objects.iter()
        .filter(|(_, obj)| obj.iter().any(|(interface_name, _)| interface_name == &"net.connman.iwd.Device"))
        .collect::<HashMap<_, _>>();

    println!("{}", serde_json::to_string(&network_interface_objects)?);

    Ok(())
}
