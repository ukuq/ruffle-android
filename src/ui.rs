use ruffle_core::backend::navigator::OwnedFuture;
use ruffle_core::backend::ui::{
    DialogLoaderError, DialogResultFuture, FileDialogResult, FileFilter, FontDefinition,
    FullscreenError, LanguageIdentifier, MouseCursor, MultiDialogResultFuture,
    MultiFileDialogResult, UiBackend, US_ENGLISH,
};
use ruffle_core::font::{FontFileData, FontMetrics, FontQuery, FontRenderer, Glyph};
use ruffle_core::swf::Twips;
use ruffle_render::bitmap::{Bitmap, BitmapFormat};
use std::cell::RefCell;
use std::convert::TryInto;
use std::sync::Arc;
use url::Url;

use crate::get_jvm;
use crate::java::JavaInterface;
use jni::objects::{JByteArray, JClass, JIntArray, JObject, JString, JValue, ReleaseMode};
use jni::sys::{jboolean, JNI_FALSE, JNI_TRUE};

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

    fn display_root_movie_download_failed_message(&self, invalid_swf: bool, fetched_error: String) {
        let reason = if invalid_swf {
            "\u{4e0d}\u{662f}\u{6709}\u{6548}\u{7684} SWF \u{6587}\u{4ef6}"
        } else if fetched_error.to_lowercase().contains("domain") {
            "\u{57df}\u{540d}\u{89e3}\u{6790}\u{5931}\u{8d25}\u{ff0c}\u{8bf7}\u{68c0}\u{67e5}\u{624b}\u{673a}\u{7f51}\u{7edc}/DNS \u{6216}\u{66f4}\u{6362}\u{53ef}\u{8bbf}\u{95ee}\u{7684}\u{94fe}\u{63a5}"
        } else {
            "\u{4e0b}\u{8f7d}\u{5931}\u{8d25}"
        };
        let message =
            format!("\u{52a0}\u{8f7d} SWF \u{5931}\u{8d25}\u{ff1a}{reason}\n{fetched_error}");
        with_android_activity(|env, activity| {
            JavaInterface::show_load_error(env, activity, &message);
        });
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
        if let Some(font_renderer) =
            AndroidCanvasFontRenderer::new(query.name.clone(), query.is_bold, query.is_italic)
        {
            register(FontDefinition::ExternalRenderer {
                name: query.name.clone(),
                is_bold: query.is_bold,
                is_italic: query.is_italic,
                font_renderer: Box::new(font_renderer),
            });
            return;
        }

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

#[derive(Debug)]
struct AndroidCanvasFontRenderer {
    family: String,
    is_bold: bool,
    is_italic: bool,
    metrics: FontMetrics,
}

impl AndroidCanvasFontRenderer {
    const SCALE: f32 = 64.0 * 20.0;
    const GLYPH_HEADER_BYTES: usize = 20;

    fn new(family: String, is_bold: bool, is_italic: bool) -> Option<Self> {
        let metrics = android_font_metrics(&family, is_bold, is_italic)?;

        Some(Self {
            family,
            is_bold,
            is_italic,
            metrics,
        })
    }
}

impl FontRenderer for AndroidCanvasFontRenderer {
    fn scale(&self) -> f32 {
        Self::SCALE
    }

    fn get_font_metrics(&self) -> FontMetrics {
        self.metrics
    }

    fn has_kerning_info(&self) -> bool {
        true
    }

    fn render_glyph(&self, character: char) -> Option<Glyph> {
        let bytes = android_render_glyph(&self.family, self.is_bold, self.is_italic, character)?;
        if bytes.len() < Self::GLYPH_HEADER_BYTES {
            log::warn!("Android glyph renderer returned a truncated glyph");
            return None;
        }

        let width = read_i32_le(&bytes, 0)?;
        let height = read_i32_le(&bytes, 4)?;
        let advance = read_i32_le(&bytes, 8)?;
        let tx = read_i32_le(&bytes, 12)?;
        let has_native_color = read_i32_le(&bytes, 16)? != 0;
        if width <= 0 || height <= 0 {
            return None;
        }

        let width = width as u32;
        let height = height as u32;
        let expected_len =
            Self::GLYPH_HEADER_BYTES.checked_add(width as usize * height as usize * 4)?;
        if bytes.len() != expected_len {
            log::warn!(
                "Android glyph renderer returned {} bytes, expected {expected_len}",
                bytes.len()
            );
            return None;
        }

        let bitmap = Bitmap::new(
            width,
            height,
            BitmapFormat::Rgba,
            bytes[Self::GLYPH_HEADER_BYTES..].to_vec(),
        );
        Some(Glyph::from_bitmap_with_native_color(
            character,
            bitmap,
            Twips::new(advance),
            Twips::new(tx),
            has_native_color,
        ))
    }

    fn calculate_kerning(&self, left: char, right: char) -> Twips {
        android_font_kerning(&self.family, self.is_bold, self.is_italic, left, right)
            .map(Twips::new)
            .unwrap_or(Twips::ZERO)
    }
}

fn read_i32_le(bytes: &[u8], offset: usize) -> Option<i32> {
    let slice = bytes.get(offset..offset + 4)?;
    Some(i32::from_le_bytes(slice.try_into().ok()?))
}

fn android_bool(value: bool) -> jboolean {
    if value {
        JNI_TRUE
    } else {
        JNI_FALSE
    }
}

fn with_android_font_renderer<R>(
    family: &str,
    is_bold: bool,
    is_italic: bool,
    f: impl FnOnce(&mut jni::JNIEnv<'_>, JClass<'_>, JString<'_>, jboolean, jboolean) -> Option<R>,
) -> Option<R> {
    let (jvm, activity) = get_jvm().ok()?;
    let mut env = jvm.attach_current_thread().ok()?;
    let class = android_font_renderer_class(&mut env, &activity)?;
    let family = env.new_string(family).ok()?;
    f(
        &mut env,
        class,
        family,
        android_bool(is_bold),
        android_bool(is_italic),
    )
}

fn android_font_renderer_class<'local>(
    env: &mut jni::JNIEnv<'local>,
    activity: &JObject<'_>,
) -> Option<JClass<'local>> {
    let class_loader =
        match env.call_method(activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[]) {
            Ok(value) => value.l().ok()?,
            Err(error) => {
                clear_pending_jni_exception(env, "getting Android class loader");
                log::warn!("Failed to get Android class loader: {error}");
                return None;
            }
        };
    let class_name = env.new_string("rs.ruffle.AndroidFontRenderer").ok()?;
    match env.call_method(
        &class_loader,
        "loadClass",
        "(Ljava/lang/String;)Ljava/lang/Class;",
        &[JValue::Object(&class_name)],
    ) {
        Ok(value) => value.l().ok().map(JClass::from),
        Err(error) => {
            clear_pending_jni_exception(env, "loading Android font renderer class");
            log::warn!("Failed to load Android font renderer class: {error}");
            None
        }
    }
}

fn clear_pending_jni_exception(env: &mut jni::JNIEnv<'_>, context: &str) {
    match env.exception_check() {
        Ok(true) => {
            let _ = env.exception_clear();
            log::warn!("Cleared Java exception while {context}");
        }
        Ok(false) => {}
        Err(error) => log::warn!("Failed to check Java exception while {context}: {error}"),
    }
}

fn android_font_metrics(family: &str, is_bold: bool, is_italic: bool) -> Option<FontMetrics> {
    with_android_font_renderer(
        family,
        is_bold,
        is_italic,
        |env, class, family, is_bold, is_italic| {
            let result = env
                .call_static_method(
                    class,
                    "metrics",
                    "(Ljava/lang/String;ZZ)[I",
                    &[
                        JValue::Object(&family),
                        JValue::Bool(is_bold),
                        JValue::Bool(is_italic),
                    ],
                )
                .map_err(|error| {
                    clear_pending_jni_exception(env, "calling Android font metrics");
                    log::warn!("Failed to call Android font metrics: {error}");
                })
                .ok()?
                .l()
                .ok()?;
            if result.is_null() {
                return None;
            }
            let metrics = JIntArray::from(result);
            let elements = unsafe {
                env.get_array_elements(&metrics, ReleaseMode::NoCopyBack)
                    .ok()?
            };
            if elements.len() < 3 {
                return None;
            }
            Some(FontMetrics {
                scale: AndroidCanvasFontRenderer::SCALE,
                ascent: elements[0],
                descent: elements[1],
                leading: elements[2] as i16,
            })
        },
    )
}

fn android_render_glyph(
    family: &str,
    is_bold: bool,
    is_italic: bool,
    character: char,
) -> Option<Vec<u8>> {
    with_android_font_renderer(
        family,
        is_bold,
        is_italic,
        |env, class, family, is_bold, is_italic| {
            let codepoint = character as i32;
            let result = env
                .call_static_method(
                    class,
                    "renderGlyph",
                    "(Ljava/lang/String;ZZI)[B",
                    &[
                        JValue::Object(&family),
                        JValue::Bool(is_bold),
                        JValue::Bool(is_italic),
                        JValue::Int(codepoint),
                    ],
                )
                .map_err(|error| {
                    clear_pending_jni_exception(env, "calling Android glyph renderer");
                    log::warn!("Failed to call Android glyph renderer: {error}");
                })
                .ok()?
                .l()
                .ok()?;
            if result.is_null() {
                return None;
            }
            let bytes = JByteArray::from(result);
            env.convert_byte_array(bytes).ok()
        },
    )
}

fn android_font_kerning(
    family: &str,
    is_bold: bool,
    is_italic: bool,
    left: char,
    right: char,
) -> Option<i32> {
    with_android_font_renderer(
        family,
        is_bold,
        is_italic,
        |env, class, family, is_bold, is_italic| {
            env.call_static_method(
                class,
                "kerning",
                "(Ljava/lang/String;ZZII)I",
                &[
                    JValue::Object(&family),
                    JValue::Bool(is_bold),
                    JValue::Bool(is_italic),
                    JValue::Int(left as i32),
                    JValue::Int(right as i32),
                ],
            )
            .map_err(|error| {
                clear_pending_jni_exception(env, "calling Android font kerning");
                log::warn!("Failed to call Android font kerning: {error}");
            })
            .ok()?
            .i()
            .ok()
        },
    )
}
