// TODO: Consider using an io_uring-based async runtime. Mostly for the sake of trying it out. And
// note that this is probably a linux-only piece of software as it leverages iwd.
// TODO: take a signal to print out the currently-watched link statuses
use async_std::task::{self, spawn};
use async_std::stream::StreamExt;
use async_std::sync::Arc;
use futures::executor::LocalSpawner;
use futures::future::join_all;
use futures::task::{SpawnExt, LocalSpawnExt};
use futures::{SinkExt, FutureExt, pin_mut};
use futures::channel::mpsc;
use zbus::{fdo, Connection};
use zbus::zvariant::{ObjectPath, OwnedObjectPath};
use dashmap::DashSet;
use futures::stream::FuturesUnordered;

mod networkd_manager;
mod systemd_manager;
mod systemd_device;

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

const NET_DEVICE_UNIT_PREFIX: &str = "sys-subsystem-net-devices-";

#[derive(Debug)]
enum Event {
    // TODO: the types of UnitRemoved and UnitNew should probably be OwnedObjectPath(ObjectPath) or
    // whatever comes in the signal we receive. I.e. we shouldn't convert them to strings where
    // they're received.
    UnitRemoved(OwnedObjectPath),
    UnitNew(OwnedObjectPath),
}

#[async_std::main]
async fn main() -> std::result::Result<(), Error> {
    let connection = Connection::system().await?;

    // 1. listen on org.freedesktop.systemd1 /org/freedesktop/systemd1 interface
    //    org.freedesktop.systemd1.Manager signal: UnitNew for any new units implementing
    //    org.freedesktop.systemd1.Device. Filter these by type, *perhaps* using
    //    https://github.com/pop-os/sysfs-class. Add network devices to a collection that we will
    //    monitor.
    // 2. listen on org.freedesktop.systemd1 /org/freedesktop/systemd1 interface
    //    org.freedesktop.systemd1.Manager signal: UnitRemoved for any removed units implementing
    //    org.freedesktop.systemd1.Device. Remove any that are in our monitoring collection.
    // 3. query systemd system bus for objects that implement org.freedesktop.systemd1.Device and
    //    are network devices. Add these to our monitoring collection.
    // 4. monitor using org.freedesktop.network1 (and perhaps for auxiliary information
    //    net.connman.iwd).
    // 5. stream jsonl data to stdout (unbuffered)

    let systemd_manager_proxy = systemd_manager::ManagerProxy::new(&connection).await?;
    let networkd_manager_proxy = networkd_manager::ManagerProxy::new(&connection).await?;

    // TODO: is it possible/useful to use receive_new_unit_with_args :
    // let mut systemd_new_units_stream = systemd_manager_proxy.receive_unit_new_with_args(&[]).await?;
    // https://docs.rs/zbus/latest/zbus/struct.Proxy.html#method.receive_signal_with_args
    // Works:
    // let mut systemd_new_units_stream = systemd_manager_proxy.receive_unit_new_with_args(&[
    //     (0, "sys-subsystem-net-devices-wg\\x2dmullvad.device")]).await?;
    // Doesn't work:
    // let mut systemd_new_units_stream = systemd_manager_proxy.receive_unit_new_with_args(&[
    //     (0, "sys-subsystem-net-devices.*")]).await?;
    let mut systemd_removed_units_stream = systemd_manager_proxy.receive_unit_removed().await?;
    let mut systemd_new_units_stream = systemd_manager_proxy.receive_unit_new().await?;

    let mut watched_units = Arc::new(DashSet::<String>::new());

    let (unit_event_send, mut unit_event_recv) = mpsc::unbounded::<Event>();

    async fn get_link_name_from_path(conn: &Connection, path: ObjectPath<'_>) -> String {
        let device_proxy = systemd_device::DeviceProxy::builder(conn)
            .path(path)
            .expect("Expected DeviceProxy builder to have correct default parameters")
            .build()
            .await
            .expect("Found object not a device");
        device_proxy
            .sys_fspath()
            .await
            .expect("Failed to fetch sysfs path")
    }

    let pool = FuturesUnordered::new(); // This can also be constructed by collecting

    async fn start_unit_watch() {

    }

    async fn reload_watched_units(
        connection: &Connection,
        networkd_manager_proxy: &networkd_manager::ManagerProxy<'_>,
    ) -> std::result::Result<(), Error> {

        let network_links = networkd_manager_proxy.list_links().await?;

        network_links.into_iter()
            .for_each(|link| {
                let connection = connection.clone();

                let handle = async move {
                    let failure_msg = &format!("Failed to create property change stream to {}", link.object_path);
                    let properties_proxy = fdo::PropertiesProxy::builder(&connection)
                        .destination("org.freedesktop.network1").expect(failure_msg)
                        .path(link.object_path.clone()).expect(failure_msg)
                        .build().await.expect(failure_msg);
                    let mut changes_stream = properties_proxy.receive_properties_changed().await.expect(failure_msg);
                    while let Some(change) = changes_stream.next().await {
                        let args = change.args().expect("Failed to get property change arguments");

                        for (name, value) in args.changed_properties().iter() {

                            println!("{} changed to {:?}", name, value);

                            // match *name {
                            //     "Percentage" => state.percentage = value.try_into()?,
                            //     "TimeToEmpty" => state.time_to_empty_seconds = value.try_into()?,
                            //     "EnergyRate" => state.energy_rate_watts = value.try_into()?,
                            //     "TimeToFull" => state.time_to_full_seconds = value.try_into()?,
                            //     "State" => state.state = value.try_into()?,
                            //     "Capacity" => state.capacity_percent = value.try_into()?,
                            //     "BatteryLevel" => state.battery_level = value.try_into()?,
                            //     _ => (),
                            // }
                        }
                    }
                };

                // (
                //     link.name,
                //     handle,
                // )
            });
        Ok(())
    }

    // watched_links.for_each(|task| { pool.spawner().spawn(task); });
    // let watched_links = .iter().collect::<DashMap<String, ()>>();

    // let watched_links = systemd_manager_proxy.list_units_by_patterns(
    //     &["loaded", "active", "plugged"],
    //     &[&format!("{NET_DEVICE_UNIT_PREFIX}*")])
    //     .await?
    //     .iter()
    //     .map(|u| (u.owned_object_path.to_string(), ()))
    //     .collect::<DashMap<String, ()>>();
    //
    // let watched_links = Arc::new(watched_links);

    let new_units_task = {
        // let watched_links = watched_links.clone();
        let connection = connection.clone();
        task::spawn(async move {
            while let Some(new_unit) = systemd_new_units_stream.next().await {
                let unit_id = new_unit.args().expect("").id;

                if unit_id.starts_with(NET_DEVICE_UNIT_PREFIX) {
                    reload_watched_units(&connection, &networkd_manager_proxy, spawner);

                    println!("{:?}", unit_id);
                }
                // let link_name = get_link_name_from_path(&connection, object_path).await;
                // watched_links.insert(link_name, ());
            }
        })
    };

    let removed_units_task = {
        // let mut send_ev = unit_event_send.clone();
        // let watched_links = watched_links.clone();
        task::spawn(async move {
            while let Some(removed_unit) = systemd_removed_units_stream.next().await {
                let object_path = removed_unit.args().expect("Failed to retrieve new unit event arguments").unit.clone();
                // watched_links.remove(&object_path.to_string());
                // send_ev.send(Event::UnitRemoved(object_path.into())).await.expect("Event channel failed");
            }
        })
    };

    // let network_devices = systemd_manager_proxy.list_units().await?;
    // TODO: does this filter work with virtual devices? e.g. when we run `mullvad connect`? Should
    //       it? Perhaps optionally?
    //       Answers:
    //       - yes it works with virtual devices
    //       - yes it should work
    //       - yes it should be optional
    //       Another note: when plugging an internet-sharing USB device (e.g. a smartphone) to the
    //       machine, a new network is created. This should be shown.
    // TODO: map the result of this to take u.6 (more importantly, this is the)

    // println!("{:?}", watched_links);

    // let initial_network_device_units = futures::future::join_all(
    //     watched_links.iter()
    //         .map(|path| {
    //             let connection = &connection;
    //             {
    //                 async move {
    //                     let device_proxy = systemd_device::DeviceProxy::builder(connection)
    //                         .path(path)
    //                         .expect("Expected DeviceProxy builder to have correct default parameters")
    //                         .build()
    //                         .await
    //                         .ok()?;
    //                     device_proxy
    //                         .sys_fspath()
    //                         .await
    //                         .ok()
    //                 }
    //                 // async move {
    //                 //     let device_proxy = systemd_device::DeviceProxy::builder(connection)
    //                 //         .path(path);
    //                 //     unit_event_send.send(Event::UnitNew(device_path)).await.expect("Event channel failed");
    //                 //     return Some(());
    //                 // }
    //             }
    //         })
    // ).await; //.iter().filter(|&unit| unit.is_some()).collect::<Vec<_>>();

    // let units = initial_network_device_units.iter()
    //     .filter(|&unit| unit.is_some())
    //     .map(|&unit| unit.unwrap())
    //     .collect::<DashMap<_, _>>();

    // println!("{:?}", initial_network_device_units);

    for task in [new_units_task, removed_units_task] {
        task.await;
    }

    println!("Running pool");

    pool.run();

    while let Some(ev) = unit_event_recv.next().await {
        match ev {
            Event::UnitNew(new_unit) => {
                // - Try to get link by name
                // - Start a task to listen for changes on the relevant link, if we aren't already
                //   listening
                // - Put the link name, the systemd object path, and the listen task into a hashmap
                //   keyed on the object path (?) so they can be easily identified when a unit is
                //   removed
                println!("Unit added {:?}", new_unit);
            }
            Event::UnitRemoved(removed_unit) => {
                // Kill the thread that listens for changes on the relevant link, if we're
                // currently listening
                println!("Unit removed {:?}", removed_unit);
            }
        }
                // if let Ok(device_proxy) = systemd_device::DeviceProxy::builder(connection)
                //     .path(object_path)
                //     .expect("Expected DeviceProxy builder to have correct default parameters")
                //     .build()
                //     .await {
                //     if let Ok(device_sysfs_path) = device_proxy
                //         .sys_fspath()
                //         .await {
                //         println!("New: {:?}", device_sysfs_path);
                //     }
                // }
    }

    // Attempt 4 to use systemd-networkd

    // let properties_proxy = fdo::PropertiesProxy::builder(&connection)
    //     .destination("org.freedesktop.network1")?
    //     .path("/org/freedesktop/network1/link")?
    //     .build()
    //     .await?;
    //
    // let mut changes_stream = properties_proxy.receive_properties_changed().await?;
    //
    // while let Some(change) = changes_stream.next().await {
    //     let args = change.args()?;
    //
    //     for (name, value) in args.changed_properties().iter() {
    //
    //         println!("{} changed to {:?}", name, value);
    //
    //         // match *name {
    //         //     "Percentage" => state.percentage = value.try_into()?,
    //         //     "TimeToEmpty" => state.time_to_empty_seconds = value.try_into()?,
    //         //     "EnergyRate" => state.energy_rate_watts = value.try_into()?,
    //         //     "TimeToFull" => state.time_to_full_seconds = value.try_into()?,
    //         //     "State" => state.state = value.try_into()?,
    //         //     "Capacity" => state.capacity_percent = value.try_into()?,
    //         //     "BatteryLevel" => state.battery_level = value.try_into()?,
    //         //     _ => (),
    //         // }
    //     }
    //
    //     // println!("{}", serde_json::to_string(&state)?);
    // }

    // Attempt 3 or something to use systemd-networkd
    //
    // let link_introspectable = fdo::IntrospectableProxy::builder(&connection)
    //     .destination("org.freedesktop.network1")?
    //     .path("/org/freedesktop/network1")?
    //     .build()
    //     .await?;
    //
    // let mut link_changes = link_introspectable.receive_all_signals().await?;
    //
    // while let Some(change) = link_changes.next().await {
    //     println!("Some change");
    //     println!("{:?}", change);
    // }

    // let object_manager_proxy = fdo::ObjectManagerProxy::builder(&connection)
    //     .destination("org.freedesktop.network1")?
    //     .path("/")?
    //     .build()
    //     .await?;
    //
    // let managed_objects = object_manager_proxy.get_managed_objects().await?;
    //
    // println!("{:?}", managed_objects);


    // let manager = manager::ManagerProxy::new(&connection).await?;
    //
    // let links = manager.list_links().await?;
    //
    // let properties_proxy = fdo::PropertiesProxy::builder(&connection)
    //     .destination("org.freedesktop.network1")?
    //     .path("/org/freedesktop/network1")?
    //     .build()
    //     .await?;
    //
    // println!("{:?}", links);
    //
    // let mut changes_stream = properties_proxy.receive_properties_changed().await?;
    //
    // while let Some(change) = changes_stream.next().await {
    //     let args = change.args()?;
    //
    //     for (name, value) in args.changed_properties().iter() {
    //
    //         println!("{} changed to {:?}", name, value);
    //
    //         // match *name {
    //         //     "Percentage" => state.percentage = value.try_into()?,
    //         //     "TimeToEmpty" => state.time_to_empty_seconds = value.try_into()?,
    //         //     "EnergyRate" => state.energy_rate_watts = value.try_into()?,
    //         //     "TimeToFull" => state.time_to_full_seconds = value.try_into()?,
    //         //     "State" => state.state = value.try_into()?,
    //         //     "Capacity" => state.capacity_percent = value.try_into()?,
    //         //     "BatteryLevel" => state.battery_level = value.try_into()?,
    //         //     _ => (),
    //         // }
    //     }
    //
    //     // println!("{}", serde_json::to_string(&state)?);
    // }

    // println!("{}", serde_json::to_string(&network_interface_objects)?);

    //
    // let managed_objects = object_manager_proxy.get_managed_objects().await?;
    //
    // let network_interface_objects = managed_objects.iter()
    //     .filter(|(_, obj)| obj.iter().any(|(interface_name, _)| interface_name == &"net.connman.iwd.Device"))
    //     .collect::<HashMap<_, _>>();
    //
    // // TODO: what should be the args??
    // let removed_interfaces_stream = object_manager_proxy.receive_interfaces_removed_with_args(&[]).await?;
    // let mut added_interfaces_stream = object_manager_proxy
    //     .receive_interfaces_added_with_args(&[]).await
    //     .expect("Must be able to receive interfaces added");
    //
    // task::spawn(async move {
    //     while let Some(change) = added_interfaces_stream.next().await {
    //         let args = change.args();
    //
    //         // for (name, value) in args.changed_properties().iter() {
    //         //     match *name {
    //         //         "Percentage" => state.percentage = value.try_into()?,
    //         //         "TimeToEmpty" => state.time_to_empty_seconds = value.try_into()?,
    //         //         "EnergyRate" => state.energy_rate_watts = value.try_into()?,
    //         //         "TimeToFull" => state.time_to_full_seconds = value.try_into()?,
    //         //         "State" => state.state = value.try_into()?,
    //         //         "Capacity" => state.capacity_percent = value.try_into()?,
    //         //         "BatteryLevel" => state.battery_level = value.try_into()?,
    //         //         _ => (),
    //         //     }
    //         // }
    //
    //         println!("{}", serde_json::to_string(&state).expect("Failed to serialize JSON"));
    //     }
    // });

    // Test that interfaces are added and removed correctly by unloading and reloading the kernel
    // module for the wifi

    // Get all devices and start a thread for each listening for changes to the device
    // Each thread listening for changes should put changes into a channel with multiple producers
    // and a single consumer.
    //
    // The channel should be consumed by the 
    //
    // Start another thread that listens for changes to the enumerated devices, using
    // object_manager_proxy.receive_signal_with_args. Kill and create listener threads as devices
    // are added and removed. The main function should end only when this thread dies.

    // println!("{}", serde_json::to_string(&network_interface_objects)?);

    Ok(())
}
