//! 托盘界面文字：跟随 Windows 显示语言，提供简体中文、英文和韩文。

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetUserDefaultUILanguage() -> u16;
}

const LANG_CHINESE: u16 = 0x04;
const LANG_KOREAN: u16 = 0x12;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Language {
    Chinese,
    English,
    Korean,
}

/// 不带参数的界面文字。
pub(crate) struct Text {
    pub(crate) connecting: &'static str,
    pub(crate) running: &'static str,
    pub(crate) exiting: &'static str,
    pub(crate) worker_stopped: &'static str,
    pub(crate) temperature_unavailable: &'static str,
    pub(crate) temperature_not_installed: &'static str,
    pub(crate) reconnect: &'static str,
    pub(crate) open_config: &'static str,
    pub(crate) install_temperature: &'static str,
    pub(crate) start_at_login: &'static str,
    pub(crate) exit: &'static str,
    pub(crate) install_cancelled: &'static str,
}

const CHINESE: Text = Text {
    connecting: "LoopPanel：正在连接",
    running: "LoopPanel：运行中",
    exiting: "LoopPanel：正在退出",
    worker_stopped: "显示线程已经停止",
    temperature_unavailable: "CPU 温度：服务暂时不可用",
    temperature_not_installed: "CPU 温度：尚未安装温度支持",
    reconnect: "重新连接",
    open_config: "打开配置文件",
    install_temperature: "安装 CPU 温度支持…",
    start_at_login: "登录时自动启动",
    exit: "退出",
    install_cancelled: "CPU 温度支持安装已取消，LoopPanel 未启动",
};

const ENGLISH: Text = Text {
    connecting: "LoopPanel: connecting",
    running: "LoopPanel: running",
    exiting: "LoopPanel: exiting",
    worker_stopped: "The display thread has stopped",
    temperature_unavailable: "CPU temperature: service temporarily unavailable",
    temperature_not_installed: "CPU temperature: support not installed",
    reconnect: "Reconnect",
    open_config: "Open configuration file",
    install_temperature: "Install CPU temperature support…",
    start_at_login: "Start at login",
    exit: "Exit",
    install_cancelled: "CPU temperature support installation was cancelled; LoopPanel did not start",
};

const KOREAN: Text = Text {
    connecting: "LoopPanel: 연결 중",
    running: "LoopPanel: 실행 중",
    exiting: "LoopPanel: 종료 중",
    worker_stopped: "디스플레이 스레드가 중지되었습니다",
    temperature_unavailable: "CPU 온도: 서비스를 일시적으로 사용할 수 없음",
    temperature_not_installed: "CPU 온도: 온도 지원이 설치되지 않음",
    reconnect: "다시 연결",
    open_config: "설정 파일 열기",
    install_temperature: "CPU 온도 지원 설치…",
    start_at_login: "로그인 시 자동 시작",
    exit: "종료",
    install_cancelled: "CPU 온도 지원 설치가 취소되어 LoopPanel을 시작하지 않았습니다",
};

impl Language {
    pub(crate) fn current() -> Self {
        Self::from_language_id(unsafe { GetUserDefaultUILanguage() })
    }

    /// 只看主语言；中文和韩文以外的语言一律使用英文。
    fn from_language_id(language_id: u16) -> Self {
        match language_id & 0x3ff {
            LANG_CHINESE => Self::Chinese,
            LANG_KOREAN => Self::Korean,
            _ => Self::English,
        }
    }

    pub(crate) fn text(self) -> &'static Text {
        match self {
            Self::Chinese => &CHINESE,
            Self::English => &ENGLISH,
            Self::Korean => &KOREAN,
        }
    }

    pub(crate) fn connection_failed(self, error: &str) -> String {
        match self {
            Self::Chinese => format!("LoopPanel：连接失败（{error}）"),
            Self::English => format!("LoopPanel: connection failed ({error})"),
            Self::Korean => format!("LoopPanel: 연결 실패({error})"),
        }
    }

    pub(crate) fn temperature(self, celsius: u32) -> String {
        match self {
            Self::Chinese => format!("CPU 温度：{celsius}°C"),
            Self::English => format!("CPU temperature: {celsius}°C"),
            Self::Korean => format!("CPU 온도: {celsius}°C"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Language;

    #[test]
    fn selects_language_from_the_primary_language_id() {
        // zh-CN、zh-TW、ko-KR、en-US、ja-JP。
        assert_eq!(Language::from_language_id(0x0804), Language::Chinese);
        assert_eq!(Language::from_language_id(0x0404), Language::Chinese);
        assert_eq!(Language::from_language_id(0x0412), Language::Korean);
        assert_eq!(Language::from_language_id(0x0409), Language::English);
        assert_eq!(Language::from_language_id(0x0411), Language::English);
    }

    #[test]
    fn formats_parameterised_text_in_each_language() {
        assert_eq!(Language::Chinese.temperature(54), "CPU 温度：54°C");
        assert_eq!(Language::English.temperature(54), "CPU temperature: 54°C");
        assert_eq!(Language::Korean.temperature(54), "CPU 온도: 54°C");
        assert_eq!(
            Language::Korean.connection_failed("x"),
            "LoopPanel: 연결 실패(x)"
        );
    }
}
