use quicknote_stack_slint::{core::ActivationCommand, store::ReminderRecord};
use std::path::Path;
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::DateTime;
use windows::UI::Notifications::{ScheduledToastNotification, ToastNotificationManager};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize, IPersistFile, StructuredStorage::PROPVARIANT,
};
use windows::Win32::UI::Shell::{
    IShellLinkW, PropertiesSystem::IPropertyStore, SetCurrentProcessExplicitAppUserModelID,
    ShellLink,
};
use windows::core::{GUID, HSTRING, Interface, PCWSTR, Result as WindowsResult};
use winreg::RegKey;
use winreg::enums::HKEY_CURRENT_USER;

pub const AUMID: &str = "QuickNote.SlintIntegrationSpike";
pub const PROTOCOL: &str = "quicknote-spike";

/// Gives every launched process the same stable identity used by Windows notifications.
pub fn set_current_process_identity() -> WindowsResult<()> {
    let aumid = HSTRING::from(AUMID);
    // SAFETY: The HSTRING remains alive for the duration of this synchronous shell call.
    unsafe { SetCurrentProcessExplicitAppUserModelID(PCWSTR(aumid.as_ptr())) }
}

/// Installs per-user protocol and notification identity registrations for this spike build.
pub fn register_current_executable() -> Result<(), Box<dyn std::error::Error>> {
    let executable = std::env::current_exe()?;
    register_executable(&executable)
}

pub fn register_executable(executable: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let classes = hkcu.create_subkey("Software\\Classes")?.0;
    let protocol = classes.create_subkey(PROTOCOL)?.0;
    protocol.set_value("", &"URL:QuickNote Spike Protocol")?;
    protocol.set_value("URL Protocol", &"")?;
    let command = protocol.create_subkey("shell\\open\\command")?.0;
    command.set_value("", &format!("\"{}\" \"%1\"", executable.display()))?;

    let identity = classes.create_subkey(format!("AppUserModelId\\{AUMID}"))?.0;
    identity.set_value("DisplayName", &"QuickNote Slint Integration Spike")?;
    identity.set_value("IconUri", &executable.to_string_lossy().to_string())?;
    create_start_menu_shortcut(executable)?;
    Ok(())
}

pub fn unregister() -> Result<(), Box<dyn std::error::Error>> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let classes = hkcu.open_subkey_with_flags("Software\\Classes", winreg::enums::KEY_WRITE)?;
    let _ = classes.delete_subkey_all(PROTOCOL);
    let _ = classes.delete_subkey_all(format!("AppUserModelId\\{AUMID}"));
    if let Some(app_data) = std::env::var_os("APPDATA") {
        let _ = std::fs::remove_file(
            Path::new(&app_data)
                .join("Microsoft\\Windows\\Start Menu\\Programs\\QuickNote Slint Spike.lnk"),
        );
    }
    Ok(())
}

fn create_start_menu_shortcut(executable: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let app_data = std::env::var_os("APPDATA").ok_or("APPDATA is unavailable.")?;
    let shortcut = Path::new(&app_data)
        .join("Microsoft\\Windows\\Start Menu\\Programs\\QuickNote Slint Spike.lnk");
    let executable_text = HSTRING::from(executable.to_string_lossy().as_ref());
    let working_directory = HSTRING::from(
        executable
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_string_lossy()
            .as_ref(),
    );
    let shortcut_text = HSTRING::from(shortcut.to_string_lossy().as_ref());

    // The shortcut must carry System.AppUserModel.ID; a plain .lnk plus registry key is
    // insufficient for reliable unpackaged desktop notification attribution.
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
    let result = (|| -> WindowsResult<()> {
        // SAFETY: COM is initialized on this thread and every string outlives its call.
        unsafe {
            let shell_link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            shell_link.SetPath(PCWSTR(executable_text.as_ptr()))?;
            shell_link.SetWorkingDirectory(PCWSTR(working_directory.as_ptr()))?;

            let property_store: IPropertyStore = shell_link.cast()?;
            let app_id_key = PROPERTYKEY {
                fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
                pid: 5,
            };
            let app_id = PROPVARIANT::from(AUMID);
            property_store.SetValue(&app_id_key, &app_id)?;
            property_store.Commit()?;

            let persist_file: IPersistFile = shell_link.cast()?;
            persist_file.Save(PCWSTR(shortcut_text.as_ptr()), true)?;
            Ok(())
        }
    })();
    // SAFETY: This balances the successful CoInitializeEx call on the same thread.
    unsafe { CoUninitialize() };
    result.map_err(Into::into)
}

pub fn schedule_reminder(reminder: &ReminderRecord) -> WindowsResult<()> {
    let document = notification_document(reminder)?;
    let delivery_time = unix_to_windows_datetime(reminder.due_at);
    let scheduled =
        ScheduledToastNotification::CreateScheduledToastNotification(&document, delivery_time)?;
    scheduled.SetId(&HSTRING::from(format!("reminder-{}", reminder.id)))?;
    scheduled.SetTag(&HSTRING::from(format!("reminder-{}", reminder.id)))?;
    scheduled.SetGroup(&HSTRING::from("quicknote-spike"))?;
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))?;
    // Replace an earlier spike schedule with the same ID before registering the new one.
    for existing in notifier.GetScheduledToastNotifications()? {
        if existing.Id()?.to_string() == format!("reminder-{}", reminder.id) {
            notifier.RemoveFromSchedule(&existing)?;
        }
    }
    notifier.AddToSchedule(&scheduled)
}

pub fn scheduled_count() -> WindowsResult<u32> {
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))?
        .GetScheduledToastNotifications()?
        .Size()
}

pub fn cancel_reminder(reminder_id: i64) -> WindowsResult<()> {
    let notification_id = format!("reminder-{reminder_id}");
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))?;
    // Remove the pending platform delivery after the domain reminder is archived.
    for existing in notifier.GetScheduledToastNotifications()? {
        if existing.Id()?.to_string() == notification_id {
            notifier.RemoveFromSchedule(&existing)?;
        }
    }
    Ok(())
}

pub fn history_count() -> WindowsResult<u32> {
    ToastNotificationManager::History()?
        .GetHistoryWithId(&HSTRING::from(AUMID))?
        .Size()
}

pub fn clear_history() -> WindowsResult<()> {
    ToastNotificationManager::History()?.ClearWithId(&HSTRING::from(AUMID))
}

fn notification_document(reminder: &ReminderRecord) -> WindowsResult<XmlDocument> {
    let open = escape_xml_attribute(
        &ActivationCommand::Open {
            note_id: reminder.note_id,
            reminder_id: reminder.id,
            delivery_at: reminder.due_at,
        }
        .as_uri(),
    );
    let snooze = escape_xml_attribute(
        &ActivationCommand::Snooze {
            note_id: reminder.note_id,
            reminder_id: reminder.id,
            delivery_at: reminder.due_at,
        }
        .as_uri(),
    );
    let title = "QuickNote 提醒";
    let xml = format!(
        r#"<toast scenario="reminder" activationType="protocol" launch="{open}">
            <visual><binding template="ToastGeneric">
                <text>{title}</text><text>验证 Slint 的计划通知与冷启动动作</text>
            </binding></visual>
            <actions>
                <action content="稍后提醒" activationType="protocol" arguments="{snooze}" />
            </actions>
        </toast>"#,
    );
    let document = XmlDocument::new()?;
    document.LoadXml(&HSTRING::from(xml))?;
    Ok(document)
}

fn escape_xml_attribute(value: &str) -> String {
    // Protocol query separators must be escaped when embedded in toast XML attributes.
    value.replace('&', "&amp;").replace('"', "&quot;")
}

fn unix_to_windows_datetime(unix_seconds: i64) -> DateTime {
    // Windows Runtime DateTime uses 100 ns intervals since 1601-01-01 UTC.
    const WINDOWS_TO_UNIX_SECONDS: i64 = 11_644_473_600;
    DateTime {
        UniversalTime: (unix_seconds + WINDOWS_TO_UNIX_SECONDS) * 10_000_000,
    }
}
