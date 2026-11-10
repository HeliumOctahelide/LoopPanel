use std::{
    mem::{size_of, zeroed},
    os::windows::ffi::OsStrExt,
    path::PathBuf,
    ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

use anyhow::{Context, Result, bail, ensure};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_CANCELLED, GetLastError, HWND, LPARAM, LRESULT, POINT, WAIT_OBJECT_0,
        WPARAM,
    },
    System::{
        LibraryLoader::GetModuleHandleW,
        Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject},
    },
    UI::{
        Shell::{
            NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
            NIM_SETFOCUS, NIM_SETVERSION, NIN_SELECT, NOTIFYICON_VERSION_4, NOTIFYICONDATAW,
            SEE_MASK_FLAG_NO_UI, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, Shell_NotifyIconW,
            ShellExecuteExW, ShellExecuteW,
        },
        WindowsAndMessaging::{
            AppendMenuW, CREATESTRUCTW, CW_USEDEFAULT, CreateIconFromResourceEx, CreatePopupMenu,
            CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu, DestroyWindow,
            DispatchMessageW, GWLP_USERDATA, GetCursorPos, GetMessageW, GetWindowLongPtrW,
            IsWindow, MB_ICONERROR, MB_OK, MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR,
            MF_STRING, MSG, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassW,
            RegisterWindowMessageW, SW_SHOWNORMAL, SetForegroundWindow, SetWindowLongPtrW,
            TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu,
            TranslateMessage, WM_APP, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_NCCREATE,
            WM_NCDESTROY, WM_NULL, WNDCLASSW, WS_EX_TOOLWINDOW, WS_OVERLAPPED,
        },
    },
};

use crate::{
    display,
    i18n::{t, tr, tr_with},
    process::DisplayLock,
    settings::{DisplaySettings, Layout, TemperatureUnit},
    startup, temperature, temperature_service,
    win::{system_dll, wide},
};

const WINDOW_CLASS: &str = "LoopPanelTrayWindow";
const ICON_BYTES: &[u8] = include_bytes!("../assets/looppanel.ico");
const ICON_ID: u32 = 1;
const WM_TRAY: u32 = WM_APP + 1;
const WM_DISPLAY_READY: u32 = WM_APP + 2;
const WM_DISPLAY_DONE: u32 = WM_APP + 3;
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;

const ID_OPEN_CONFIG: u32 = 1001;
const ID_STARTUP: u32 = 1002;
const ID_RETRY: u32 = 1003;
const ID_EXIT: u32 = 1004;
const ID_INSTALL_TEMPERATURE: u32 = 1005;
const ID_LAYOUT_DEFAULT: u32 = 1006;
const ID_LAYOUT_SIMPLE: u32 = 1007;
const ID_UNIT_CELSIUS: u32 = 1008;
const ID_UNIT_FAHRENHEIT: u32 = 1009;

pub fn run() -> Result<()> {
    let _display_lock = DisplayLock::acquire()?;
    let executable = std::env::current_exe()?;
    install_temperature_service(&executable)?;
    let config_file = executable
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("looppanel.toml");
    let taskbar_created = unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) };
    ensure!(
        taskbar_created != 0,
        "{}",
        t!("tray.error.taskbar_message_failed")
    );

    let instance = unsafe { GetModuleHandleW(ptr::null()) };
    ensure!(
        !instance.is_null(),
        "{}",
        t!("tray.error.module_handle_failed", error = last_error())
    );
    let class_name = wide(WINDOW_CLASS);
    let class = WNDCLASSW {
        style: 0,
        lpfnWndProc: Some(window_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: ptr::null_mut(),
        hCursor: ptr::null_mut(),
        hbrBackground: ptr::null_mut(),
        lpszMenuName: ptr::null(),
        lpszClassName: class_name.as_ptr(),
    };
    ensure!(
        unsafe { RegisterClassW(&class) } != 0,
        "{}",
        t!("tray.error.window_class_failed", error = last_error())
    );
    let icon = load_application_icon();
    ensure!(
        !icon.is_null(),
        "{}",
        t!("tray.error.icon_load_failed", error = last_error())
    );

    let mut state = Box::new(AppState {
        executable,
        config_file,
        display_state: DisplayState::Connecting,
        stop: Arc::new(AtomicBool::new(false)),
        settings: Arc::new(DisplaySettings::load()),
        worker: None,
        taskbar_created,
        icon,
        icon_added: false,
    });
    let state_pointer: *mut AppState = &mut *state;
    let title = wide("LoopPanel");
    let window = unsafe {
        CreateWindowExW(
            // 不能带 WS_EX_NOACTIVATE：那样 SetForegroundWindow 无法让这个隐藏窗口成为前台窗口，
            // 托盘菜单就会被压在任务栏后面。
            WS_EX_TOOLWINDOW,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPED,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            0,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            instance,
            state_pointer.cast(),
        )
    };
    ensure!(
        !window.is_null(),
        "{}",
        t!("tray.error.window_create_failed", error = last_error())
    );

    let result = (|| -> Result<()> {
        add_icon(state_pointer, window)?;
        if unsafe { (&mut *state_pointer).start_worker(window) } {
            modify_icon(state_pointer, window);
        }

        let mut message: MSG = unsafe { zeroed() };
        loop {
            let result = unsafe { GetMessageW(&mut message, ptr::null_mut(), 0, 0) };
            if result == -1 {
                bail!(
                    "{}",
                    t!("tray.error.message_read_failed", error = last_error())
                );
            }
            if result == 0 {
                break;
            }
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        Ok(())
    })();
    cleanup_window(state_pointer, window);
    result
}

pub fn show_fatal_error(error: &anyhow::Error) {
    let title = wide("LoopPanel");
    let message = wide(&format!("{error:#}"));
    unsafe {
        MessageBoxW(
            ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        )
    };
}

enum DisplayState {
    Connecting,
    Running,
    Failed(String),
    Exiting,
}

enum WorkerCompletion {
    Ignore,
    UpdateIcon,
    DestroyWindow,
}

struct AppState {
    executable: PathBuf,
    config_file: PathBuf,
    display_state: DisplayState,
    stop: Arc<AtomicBool>,
    settings: Arc<DisplaySettings>,
    worker: Option<JoinHandle<Result<()>>>,
    taskbar_created: u32,
    icon: windows_sys::Win32::UI::WindowsAndMessaging::HICON,
    icon_added: bool,
}

impl AppState {
    fn start_worker(&mut self, window: HWND) -> bool {
        if self.worker.is_some() || matches!(self.display_state, DisplayState::Exiting) {
            return false;
        }
        self.display_state = DisplayState::Connecting;
        self.stop = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&self.stop);
        let settings = Arc::clone(&self.settings);
        let window_value = window as usize;
        self.worker = Some(thread::spawn(move || {
            let result = display::run_until(None, &stop, &settings, || unsafe {
                PostMessageW(window_value as HWND, WM_DISPLAY_READY, 0, 0);
            });
            unsafe {
                PostMessageW(window_value as HWND, WM_DISPLAY_DONE, 0, 0);
            }
            result
        }));
        true
    }

    /// 显示选项保存之后，唤醒显示线程立即重画。
    fn wake_worker(&self) {
        if let Some(worker) = &self.worker {
            worker.thread().unpark();
        }
    }

    fn display_ready(&mut self) -> bool {
        if !matches!(self.display_state, DisplayState::Exiting) {
            self.display_state = DisplayState::Running;
            return true;
        }
        false
    }

    fn display_done(&mut self) -> WorkerCompletion {
        let Some(worker) = self.worker.take() else {
            return WorkerCompletion::Ignore;
        };
        let result = worker
            .join()
            .unwrap_or_else(|_| Err(anyhow::anyhow!("{}", t!("tray.error.display_thread_ended"))));
        if matches!(self.display_state, DisplayState::Exiting) {
            return WorkerCompletion::DestroyWindow;
        }
        self.display_state = DisplayState::Failed(match result {
            Ok(()) => tr("tray.status.worker_stopped"),
            Err(error) => format!("{error:#}"),
        });
        WorkerCompletion::UpdateIcon
    }

    fn request_exit(&mut self) -> bool {
        if matches!(self.display_state, DisplayState::Exiting) {
            return false;
        }
        self.display_state = DisplayState::Exiting;
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = &self.worker {
            worker.thread().unpark();
            false
        } else {
            true
        }
    }

    fn status_text(&self) -> String {
        match &self.display_state {
            DisplayState::Connecting => tr("tray.status.connecting"),
            DisplayState::Running => tr("tray.status.running"),
            DisplayState::Failed(error) => tr_with("tray.status.failed", &[("error", error)]),
            DisplayState::Exiting => tr("tray.status.exiting"),
        }
    }

    fn icon_data(&self, window: HWND) -> NOTIFYICONDATAW {
        let mut data: NOTIFYICONDATAW = unsafe { zeroed() };
        data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = window;
        data.uID = ICON_ID;
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
        data.uCallbackMessage = WM_TRAY;
        data.hIcon = self.icon;
        let tip = wide(&self.status_text());
        let length = (tip.len() - 1).min(data.szTip.len() - 1);
        data.szTip[..length].copy_from_slice(&tip[..length]);
        data
    }
}

fn show_menu(state: *mut AppState, window: HWND) -> Result<()> {
    let (status, failed, executable, config_file, settings) = unsafe {
        let state = &*state;
        (
            state.status_text(),
            matches!(state.display_state, DisplayState::Failed(_)),
            state.executable.clone(),
            state.config_file.clone(),
            Arc::clone(&state.settings),
        )
    };
    let layout = settings.layout();
    let unit = settings.temperature_unit();
    let menu = unsafe { CreatePopupMenu() };
    ensure!(
        !menu.is_null(),
        "{}",
        t!("tray.error.menu_create_failed", error = last_error())
    );
    let menu = Menu(menu);

    append_menu(menu.0, MF_STRING | MF_GRAYED, 0, &status)?;
    let temperature_installed = temperature_service::is_installed();
    let temperature = match temperature::service_sample() {
        Ok(value) => tr_with(
            "tray.temperature.value",
            &[("temperature", &unit.format(value))],
        ),
        Err(_) if temperature_installed => tr("tray.temperature.unavailable"),
        Err(_) => tr("tray.temperature.not_installed"),
    };
    append_menu(menu.0, MF_STRING | MF_GRAYED, 0, &temperature)?;
    append_menu(menu.0, MF_SEPARATOR, 0, "")?;
    if failed {
        append_menu(
            menu.0,
            MF_STRING,
            ID_RETRY as usize,
            &tr("tray.menu.reconnect"),
        )?;
    }
    append_menu(
        menu.0,
        MF_STRING,
        ID_OPEN_CONFIG as usize,
        &tr("tray.menu.open_config"),
    )?;
    if !temperature_installed {
        append_menu(
            menu.0,
            MF_STRING,
            ID_INSTALL_TEMPERATURE as usize,
            &tr("tray.menu.install_temperature"),
        )?;
    }
    append_choice_menu(
        menu.0,
        &tr("tray.menu.display"),
        &[
            (
                ID_LAYOUT_DEFAULT,
                tr("tray.menu.display_default"),
                layout == Layout::Default,
            ),
            (
                ID_LAYOUT_SIMPLE,
                tr("tray.menu.display_simple"),
                layout == Layout::Simple,
            ),
        ],
    )?;
    append_choice_menu(
        menu.0,
        &tr("tray.menu.unit"),
        &[
            (
                ID_UNIT_CELSIUS,
                tr("tray.menu.unit_celsius"),
                unit == TemperatureUnit::Celsius,
            ),
            (
                ID_UNIT_FAHRENHEIT,
                tr("tray.menu.unit_fahrenheit"),
                unit == TemperatureUnit::Fahrenheit,
            ),
        ],
    )?;
    let startup_enabled = startup::is_enabled(&executable)?;
    append_menu(
        menu.0,
        MF_STRING | if startup_enabled { MF_CHECKED } else { 0 },
        ID_STARTUP as usize,
        &tr("tray.menu.start_at_login"),
    )?;
    append_menu(menu.0, MF_SEPARATOR, 0, "")?;
    append_menu(menu.0, MF_STRING, ID_EXIT as usize, &tr("tray.menu.exit"))?;

    // 通知消息里附带的锚点是物理像素；本进程未声明 DPI 感知，TrackPopupMenu 用的是缩放后的
    // 逻辑坐标。在 200% 缩放下直接使用锚点会把菜单推到屏幕右下角，所以统一取光标位置。
    let mut point = POINT::default();
    unsafe { GetCursorPos(&mut point) };
    unsafe { SetForegroundWindow(window) };
    let command = unsafe {
        TrackPopupMenu(
            menu.0,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_LEFTALIGN,
            point.x,
            point.y,
            0,
            window,
            ptr::null(),
        )
    } as u32;
    let data = unsafe { (&*state).icon_data(window) };
    unsafe {
        PostMessageW(window, WM_NULL, 0, 0);
        Shell_NotifyIconW(NIM_SETFOCUS, &data);
    }
    match command {
        ID_OPEN_CONFIG => open_path(&config_file)?,
        ID_STARTUP => startup::set_enabled(&executable, !startup_enabled)?,
        ID_RETRY => {
            let started = unsafe { (&mut *state).start_worker(window) };
            if started {
                modify_icon(state, window);
            }
        }
        ID_INSTALL_TEMPERATURE => install_temperature_service(&executable)?,
        ID_LAYOUT_DEFAULT | ID_LAYOUT_SIMPLE | ID_UNIT_CELSIUS | ID_UNIT_FAHRENHEIT => {
            match command {
                ID_LAYOUT_DEFAULT => settings.set_layout(Layout::Default)?,
                ID_LAYOUT_SIMPLE => settings.set_layout(Layout::Simple)?,
                ID_UNIT_CELSIUS => settings.set_temperature_unit(TemperatureUnit::Celsius)?,
                _ => settings.set_temperature_unit(TemperatureUnit::Fahrenheit)?,
            }
            unsafe { (&*state).wake_worker() };
        }
        ID_EXIT => {
            let destroy = unsafe { (&mut *state).request_exit() };
            if destroy {
                unsafe { DestroyWindow(window) };
            }
        }
        _ => {}
    }
    Ok(())
}

fn install_temperature_service(executable: &std::path::Path) -> Result<()> {
    if temperature_service::is_installed() {
        return Ok(());
    }
    let service = executable.with_file_name("looppanel-temperature-service.exe");
    ensure!(
        service.is_file(),
        "{}",
        t!("tray.error.installer_not_found", path = service.display())
    );
    run_elevated_and_wait(&service, "install")?;
    ensure!(
        temperature_service::is_installed(),
        "{}",
        t!("tray.error.service_not_installed")
    );
    temperature::service_sample().with_context(|| t!("tray.error.service_not_readable"))?;
    Ok(())
}

fn run_elevated_and_wait(path: &std::path::Path, parameters: &str) -> Result<()> {
    let operation = wide("runas");
    let path = path
        .as_os_str()
        .encode_wide()
        .chain([0])
        .collect::<Vec<_>>();
    let parameters = wide(parameters);
    let mut info: SHELLEXECUTEINFOW = unsafe { zeroed() };
    info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI;
    info.lpVerb = operation.as_ptr();
    info.lpFile = path.as_ptr();
    info.lpParameters = parameters.as_ptr();
    info.nShow = SW_SHOWNORMAL;
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        let error = unsafe { GetLastError() };
        if error == ERROR_CANCELLED {
            bail!("{}", tr("tray.install.cancelled"));
        }
        bail!(
            "{}",
            t!(
                "tray.error.installer_launch_failed",
                error = std::io::Error::from_raw_os_error(error as i32)
            )
        );
    }
    ensure!(
        !info.hProcess.is_null(),
        "{}",
        t!("tray.error.installer_no_handle")
    );
    let process = ProcessHandle(info.hProcess);
    ensure!(
        unsafe { WaitForSingleObject(process.0, INFINITE) } == WAIT_OBJECT_0,
        "{}",
        t!("tray.error.installer_wait_failed", error = last_error())
    );
    let mut exit_code = 0_u32;
    ensure!(
        unsafe { GetExitCodeProcess(process.0, &mut exit_code) } != 0,
        "{}",
        t!(
            "tray.error.installer_exit_code_failed",
            error = last_error()
        )
    );
    ensure!(
        exit_code == 0,
        "{}",
        t!("tray.error.installer_failed", exit_code = exit_code)
    );
    Ok(())
}

fn add_icon(state: *mut AppState, window: HWND) -> Result<()> {
    let mut data = unsafe { (&*state).icon_data(window) };
    ensure!(
        unsafe { Shell_NotifyIconW(NIM_ADD, &data) } != 0,
        "{}",
        t!("tray.error.icon_add_failed")
    );
    unsafe { (*state).icon_added = true };
    data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
    ensure!(
        unsafe { Shell_NotifyIconW(NIM_SETVERSION, &data) } != 0,
        "{}",
        t!("tray.error.icon_version_failed")
    );
    Ok(())
}

fn modify_icon(state: *const AppState, window: HWND) {
    if unsafe { (*state).icon_added } {
        let data = unsafe { (&*state).icon_data(window) };
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
    }
}

fn remove_icon(state: *mut AppState, window: HWND) {
    if unsafe { (*state).icon_added } {
        let data = unsafe { (&*state).icon_data(window) };
        unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
        unsafe { (*state).icon_added = false };
    }
}

fn cleanup_window(state: *mut AppState, window: HWND) {
    let worker = unsafe {
        let state = &mut *state;
        state.display_state = DisplayState::Exiting;
        state.stop.store(true, Ordering::Relaxed);
        let worker = state.worker.take();
        if let Some(worker) = &worker {
            worker.thread().unpark();
        }
        worker
    };
    remove_icon(state, window);
    if unsafe { IsWindow(window) } != 0 {
        unsafe {
            SetWindowLongPtrW(window, GWLP_USERDATA, 0);
            DestroyWindow(window);
        }
    }
    if let Some(worker) = worker {
        let _ = worker.join();
    }
    unsafe { DestroyIcon((*state).icon) };
}

fn load_application_icon() -> windows_sys::Win32::UI::WindowsAndMessaging::HICON {
    let entries = u16::from_le_bytes(ICON_BYTES[4..6].try_into().unwrap()) as usize;
    let entry = (0..entries)
        .map(|index| 6 + index * 16)
        .find(|offset| ICON_BYTES[*offset] == 32 && ICON_BYTES[*offset + 1] == 32)
        .unwrap_or(6);
    let image_size = u32::from_le_bytes(ICON_BYTES[entry + 8..entry + 12].try_into().unwrap());
    let image_offset =
        u32::from_le_bytes(ICON_BYTES[entry + 12..entry + 16].try_into().unwrap()) as usize;
    unsafe {
        CreateIconFromResourceEx(
            ICON_BYTES[image_offset..].as_ptr(),
            image_size,
            1,
            0x0003_0000,
            32,
            32,
            0,
        )
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let create = unsafe { &*(lparam as *const CREATESTRUCTW) };
        unsafe { SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize) };
        return 1;
    }
    let state_pointer = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) as *mut AppState };
    if state_pointer.is_null() {
        return unsafe { DefWindowProcW(window, message, wparam, lparam) };
    }

    if message == unsafe { (*state_pointer).taskbar_created } {
        unsafe { (*state_pointer).icon_added = false };
        if let Err(error) = add_icon(state_pointer, window) {
            show_fatal_error(&error);
        }
        return 0;
    }
    match message {
        WM_TRAY => {
            let event = lparam as u32 & 0xffff;
            // NOTIFYICON_VERSION_4 下，一次点击会先送来原始的 WM_?BUTTONUP，随后才是
            // WM_CONTEXTMENU / NIN_SELECT。外壳只在后者送达时才允许本进程取得前台；
            // 若在原始按键消息上就弹出菜单，SetForegroundWindow 会失败，菜单被托盘溢出面板盖住。
            let selected = matches!(event, WM_CONTEXTMENU | NIN_SELECT | NIN_KEYSELECT);
            if selected && let Err(error) = show_menu(state_pointer, window) {
                show_fatal_error(&error);
            }
            0
        }
        WM_DISPLAY_READY => {
            let update_icon = unsafe { (&mut *state_pointer).display_ready() };
            if update_icon {
                modify_icon(state_pointer, window);
            }
            0
        }
        WM_DISPLAY_DONE => {
            let completion = unsafe { (&mut *state_pointer).display_done() };
            match completion {
                WorkerCompletion::Ignore => {}
                WorkerCompletion::UpdateIcon => modify_icon(state_pointer, window),
                WorkerCompletion::DestroyWindow => unsafe {
                    DestroyWindow(window);
                },
            }
            0
        }
        WM_CLOSE => {
            let destroy = unsafe { (&mut *state_pointer).request_exit() };
            if destroy {
                unsafe { DestroyWindow(window) };
            }
            0
        }
        WM_DESTROY => {
            remove_icon(state_pointer, window);
            unsafe { PostQuitMessage(0) };
            0
        }
        WM_NCDESTROY => unsafe {
            SetWindowLongPtrW(window, GWLP_USERDATA, 0);
            DefWindowProcW(window, message, wparam, lparam)
        },
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

/// 追加一个单选子菜单；`choices` 为（命令号，文字，是否当前选中）。
fn append_choice_menu(
    menu: windows_sys::Win32::UI::WindowsAndMessaging::HMENU,
    title: &str,
    choices: &[(u32, String, bool)],
) -> Result<()> {
    let submenu = unsafe { CreatePopupMenu() };
    ensure!(
        !submenu.is_null(),
        "{}",
        t!("tray.error.menu_create_failed", error = last_error())
    );
    // 挂到父菜单之前由这里负责销毁；挂上之后随父菜单一起销毁。
    let submenu = Menu(submenu);
    for (id, label, selected) in choices {
        append_menu(
            submenu.0,
            MF_STRING | if *selected { MF_CHECKED } else { 0 },
            *id as usize,
            label,
        )?;
    }
    append_menu(menu, MF_POPUP, submenu.0 as usize, title)?;
    std::mem::forget(submenu);
    Ok(())
}

fn append_menu(
    menu: windows_sys::Win32::UI::WindowsAndMessaging::HMENU,
    flags: u32,
    id: usize,
    text: &str,
) -> Result<()> {
    let text = wide(text);
    ensure!(
        unsafe { AppendMenuW(menu, flags, id, text.as_ptr()) } != 0,
        "{}",
        t!("tray.error.menu_item_failed", error = last_error())
    );
    Ok(())
}

fn open_path(path: &std::path::Path) -> Result<()> {
    let notepad = system_dll("notepad.exe")?;
    let parameters = format!("\"{}\"", path.display());
    execute(&notepad, Some(&parameters), "open")
}

fn execute(path: &std::path::Path, parameters: Option<&str>, operation: &str) -> Result<()> {
    let operation = wide(operation);
    let path = path
        .as_os_str()
        .encode_wide()
        .chain([0])
        .collect::<Vec<_>>();
    let parameters = parameters.map(wide);
    let result = unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            operation.as_ptr(),
            path.as_ptr(),
            parameters
                .as_ref()
                .map(|value| value.as_ptr())
                .unwrap_or(ptr::null()),
            ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    ensure!(
        result as usize > 32,
        "{}",
        t!("tray.error.config_open_failed")
    );
    Ok(())
}

fn last_error() -> std::io::Error {
    let error = unsafe { GetLastError() };
    std::io::Error::from_raw_os_error(error as i32)
}

struct Menu(windows_sys::Win32::UI::WindowsAndMessaging::HMENU);

impl Drop for Menu {
    fn drop(&mut self) {
        unsafe { DestroyMenu(self.0) };
    }
}

struct ProcessHandle(windows_sys::Win32::Foundation::HANDLE);

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}
