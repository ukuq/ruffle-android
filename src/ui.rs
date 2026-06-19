use ruffle_core::backend::navigator::OwnedFuture;
use ruffle_core::backend::ui::{
    DialogLoaderError, DialogResultFuture, FileDialogResult, FileFilter, FontDefinition,
    FullscreenError, LanguageIdentifier, MouseCursor, MultiDialogResultFuture,
    MultiFileDialogResult, UiBackend, US_ENGLISH,
};
use ruffle_core::font::{FontFileData, FontQuery};
use std::cell::RefCell;
use std::sync::Arc;
use url::Url;

use crate::get_jvm;
use crate::java::JavaInterface;

#[derive(Clone)]
struct AndroidFontSource {
    data: Arc<dyn AsRef<[u8]>>,
    index: u32,
}

pub struct AndroidUiBackend {
    font_source: RefCell<Option<AndroidFontSource>>,
}

impl AndroidUiBackend {
    pub fn new() -> Self {
        Self {
            font_source: RefCell::new(None),
        }
    }

    fn font_source(&self) -> Option<AndroidFontSource> {
        if let Some(source) = self.font_source.borrow().clone() {
            return Some(source);
        }

        let source = load_android_font_source();
        *self.font_source.borrow_mut() = source.clone();
        source
    }
}

impl UiBackend for AndroidUiBackend {
    fn mouse_visible(&self) -> bool {
        false
    }

    fn set_mouse_visible(&mut self, _visible: bool) {}

    fn set_mouse_cursor(&mut self, _cursor: MouseCursor) {}

    fn clipboard_content(&mut self) -> String {
        String::new()
    }

    fn set_clipboard_content(&mut self, _content: String) {}

    fn set_fullscreen(&mut self, _is_full: bool) -> Result<(), FullscreenError> {
        Ok(())
    }

    fn display_root_movie_download_failed_message(
        &self,
        _invalid_swf: bool,
        _fetched_error: String,
    ) {
    }

    fn message(&self, _message: &str) {}

    fn open_virtual_keyboard(&self) {
        with_android_activity(|env, activity| {
            JavaInterface::show_virtual_keyboard(env, activity);
        });
    }

    fn close_virtual_keyboard(&self) {
        with_android_activity(|env, activity| {
            JavaInterface::hide_virtual_keyboard(env, activity);
        });
    }

    fn language(&self) -> LanguageIdentifier {
        US_ENGLISH.clone()
    }

    fn display_unsupported_video(&self, _url: Url) {}

    fn load_device_font(&self, query: &FontQuery, register: &mut dyn FnMut(FontDefinition)) {
        if let Some(source) = self.font_source() {
            register(FontDefinition::FontFile {
                name: query.name.clone(),
                is_bold: query.is_bold,
                is_italic: query.is_italic,
                data: FontFileData::new_shared(source.data),
                index: source.index,
            });
        }
    }

    fn sort_device_fonts(
        &self,
        query: &FontQuery,
        register: &mut dyn FnMut(FontDefinition),
    ) -> Vec<FontQuery> {
        self.load_device_font(query, register);
        vec![query.clone()]
    }

    fn display_file_open_dialog(
        &mut self,
        _filters: Vec<FileFilter>,
    ) -> Option<DialogResultFuture> {
        Some(canceled_file_dialog())
    }

    fn display_file_open_dialog_multiple(
        &mut self,
        _filters: Vec<FileFilter>,
    ) -> Option<MultiDialogResultFuture> {
        Some(Box::pin(async move { Ok(MultiFileDialogResult::Canceled) }))
    }

    fn display_file_save_dialog(
        &mut self,
        _file_name: String,
        _title: String,
    ) -> Option<DialogResultFuture> {
        Some(canceled_file_dialog())
    }

    fn close_file_dialog(&mut self) {}
}

fn with_android_activity(f: impl FnOnce(&mut jni::JNIEnv<'_>, &jni::objects::JObject<'_>)) {
    match get_jvm() {
        Ok((jvm, activity)) => match jvm.attach_current_thread() {
            Ok(mut env) => f(&mut env, &activity),
            Err(err) => log::error!("Failed to attach JVM for virtual keyboard: {}", err),
        },
        Err(err) => log::error!("Failed to get JVM for virtual keyboard: {}", err),
    }
}

fn canceled_file_dialog() -> OwnedFuture<FileDialogResult, DialogLoaderError> {
    Box::pin(async move { Ok(FileDialogResult::Canceled) })
}

fn load_android_font_source() -> Option<AndroidFontSource> {
    const FONT_PATHS: &[(&str, u32)] = &[
        ("/system/fonts/NotoSansCJK-Regular.ttc", 2),
        ("/system/fonts/NotoSansSC-Regular.otf", 0),
        ("/system/fonts/DroidSansFallback.ttf", 0),
        ("/system/fonts/MiSansVF.ttf", 0),
        ("/product/fonts/MiSansVF.ttf", 0),
        ("/system/fonts/Roboto-Regular.ttf", 0),
    ];

    for (path, index) in FONT_PATHS {
        match std::fs::read(path) {
            Ok(data) => {
                log::info!("Loaded Android device font source: {path}#{index}");
                return Some(AndroidFontSource {
                    data: Arc::new(data),
                    index: *index,
                });
            }
            Err(err) => {
                log::debug!("Android device font source unavailable: {path}: {err}");
            }
        }
    }

    log::warn!("No Android device font source found");
    None
}
