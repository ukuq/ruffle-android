use jni::objects::{JClass, JIntArray, JMethodID, JObject, JString, JValue, ReleaseMode};
use jni::signature::{Primitive, ReturnType};
use jni::JNIEnv;
use ruffle_core::ContextMenuItem;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Handles to various items on the Java `PlayerActivity` class.
/// This is statically initialized once at startup, via the Java method `nativeInit()`.
/// This avoids needing to pay the lookup and validation penalty for every single call back into Java,
/// which can be a significant cost.
pub struct JavaInterface {
    get_surface_width: JMethodID,
    get_surface_height: JMethodID,
    show_context_menu: JMethodID,
    get_trace_output: JMethodID,
    get_loc_in_window: JMethodID,
    get_android_data_storage_dir: JMethodID,
    get_android_app_data_dir: JMethodID,
    show_virtual_keyboard: JMethodID,
    hide_virtual_keyboard: JMethodID,
}

static JAVA_INTERFACE: OnceLock<JavaInterface> = OnceLock::new();

impl JavaInterface {
    pub fn get_surface_width(env: &mut JNIEnv, this: &JObject) -> i32 {
        let result = unsafe {
            env.call_method_unchecked(
                this,
                Self::get().get_surface_width,
                ReturnType::Primitive(Primitive::Int),
                &[],
            )
        };
        result
            .expect("getSurfaceWidth() must never throw")
            .i()
            .unwrap()
    }

    pub fn get_surface_height(env: &mut JNIEnv, this: &JObject) -> i32 {
        let result = unsafe {
            env.call_method_unchecked(
                this,
                Self::get().get_surface_height,
                ReturnType::Primitive(Primitive::Int),
                &[],
            )
        };
        result
            .expect("getSurfaceHeight() must never throw")
            .i()
            .unwrap()
    }

    pub fn show_context_menu(env: &mut JNIEnv, this: &JObject, items: &[ContextMenuItem]) {
        let arr = env
            .new_object_array(items.len() as i32, "java/lang/String", JObject::null())
            .unwrap();
        for (i, e) in items.iter().enumerate() {
            let s = env
                .new_string(format!(
                    "{} {} {} {}",
                    e.enabled, e.separator_before, e.checked, e.caption
                ))
                .unwrap();
            env.set_object_array_element(&arr, i as i32, s).unwrap();
        }
        let result = unsafe {
            env.call_method_unchecked(
                this,
                Self::get().show_context_menu,
                ReturnType::Primitive(Primitive::Void),
                &[JValue::Object(&arr).as_jni()],
            )
        };
        result.expect("showContextMenu() must never throw");
    }

    pub fn get_trace_output(env: &mut JNIEnv, this: &JObject) -> Option<PathBuf> {
        let result = unsafe {
            env.call_method_unchecked(this, Self::get().get_trace_output, ReturnType::Object, &[])
        };
        let object = result
            .expect("getTraceOutput() must never throw")
            .l()
            .unwrap();
        if object.is_null() {
            return None;
        }
        let string_object = JString::from(object);
        let java_string = unsafe { env.get_string_unchecked(&string_object) };
        let url = java_string.unwrap().to_string_lossy().to_string();
        Some(url.into())
    }

    pub fn get_loc_in_window(env: &mut JNIEnv, this: &JObject) -> (i32, i32) {
        let result = unsafe {
            env.call_method_unchecked(this, Self::get().get_loc_in_window, ReturnType::Array, &[])
        };
        let object = result
            .expect("getLocInWindow() must never throw")
            .l()
            .unwrap();
        let arr = JIntArray::from(object);
        let elements = unsafe {
            env.get_array_elements(&arr, ReleaseMode::NoCopyBack)
                .unwrap()
        };
        let data = unsafe { std::slice::from_raw_parts(elements.as_ptr(), elements.len()) };
        (data[0], data[1])
    }

    pub fn get_android_data_storage_dir(env: &mut JNIEnv, this: &JObject) -> PathBuf {
        let result = unsafe {
            env.call_method_unchecked(
                this,
                Self::get().get_android_data_storage_dir,
                ReturnType::Object,
                &[],
            )
        };
        let object = result
            .expect("getAndroidDataStorageDir() must never throw")
            .l()
            .unwrap();
        let string_object = JString::from(object);
        let java_string = unsafe { env.get_string_unchecked(&string_object) };
        let path = java_string.unwrap().to_string_lossy().to_string();
        PathBuf::from(path)
    }

    pub fn get_android_app_data_dir(env: &mut JNIEnv, this: &JObject) -> PathBuf {
        let result = unsafe {
            env.call_method_unchecked(
                this,
                Self::get().get_android_app_data_dir,
                ReturnType::Object,
                &[],
            )
        };
        let object = result
            .expect("getAndroidAppDataDir() must never throw")
            .l()
            .unwrap();
        let string_object = JString::from(object);
        let java_string = unsafe { env.get_string_unchecked(&string_object) };
        let path = java_string.unwrap().to_string_lossy().to_string();
        PathBuf::from(path)
    }

    pub fn get_render_backend(env: &mut JNIEnv, this: &JObject) -> String {
        let result = env.call_method(this, "getRenderBackend", "()Ljava/lang/String;", &[]);
        let object = result
            .expect("getRenderBackend() must never throw")
            .l()
            .unwrap();
        let string_object = JString::from(object);
        let java_string = unsafe { env.get_string_unchecked(&string_object) };
        let backend = java_string.unwrap().to_string_lossy().to_string();
        backend
    }

    pub fn get_render_scale(env: &mut JNIEnv, this: &JObject) -> f32 {
        env.call_method(this, "getRenderScale", "()F", &[])
            .expect("getRenderScale() must never throw")
            .f()
            .unwrap_or(1.0)
    }

    pub fn get_stage_quality(env: &mut JNIEnv, this: &JObject) -> String {
        let result = env.call_method(this, "getStageQuality", "()Ljava/lang/String;", &[]);
        let object = result
            .expect("getStageQuality() must never throw")
            .l()
            .unwrap();
        let string_object = JString::from(object);
        let java_string = unsafe { env.get_string_unchecked(&string_object) };
        let quality = java_string.unwrap().to_string_lossy().to_string();
        quality
    }

    pub fn show_load_failure(env: &mut JNIEnv, this: &JObject, message: &str) {
        let java_message = env.new_string(message).unwrap();
        let result = env.call_method(
            this,
            "showLoadFailureAndExit",
            "(Ljava/lang/String;)V",
            &[(&java_message).into()],
        );
        result.expect("showLoadFailureAndExit() must never throw");
    }

    pub fn show_virtual_keyboard(env: &mut JNIEnv, this: &JObject) {
        let result = unsafe {
            env.call_method_unchecked(
                this,
                Self::get().show_virtual_keyboard,
                ReturnType::Primitive(Primitive::Void),
                &[],
            )
        };
        result.expect("showVirtualKeyboard() must never throw");
    }

    pub fn hide_virtual_keyboard(env: &mut JNIEnv, this: &JObject) {
        let result = unsafe {
            env.call_method_unchecked(
                this,
                Self::get().hide_virtual_keyboard,
                ReturnType::Primitive(Primitive::Void),
                &[],
            )
        };
        result.expect("hideVirtualKeyboard() must never throw");
    }

    pub fn update_server_metrics(env: &mut JNIEnv, this: &JObject, text: &str) {
        let java_text = env.new_string(text).unwrap();
        let result = env.call_method(
            this,
            "updateServerMetrics",
            "(Ljava/lang/String;)V",
            &[(&java_text).into()],
        );
        result.expect("updateServerMetrics() must never throw");
    }

    pub fn update_fps(env: &mut JNIEnv, this: &JObject, text: &str) {
        let java_text = env.new_string(text).unwrap();
        let result = env.call_method(
            this,
            "updateFps",
            "(Ljava/lang/String;)V",
            &[(&java_text).into()],
        );
        result.expect("updateFps() must never throw");
    }

    pub fn get() -> &'static JavaInterface {
        JAVA_INTERFACE
            .get()
            .expect("Java interface must have been created via nativeInit()")
    }

    pub fn init(env: &mut JNIEnv, class: &JClass) {
        let _ = JAVA_INTERFACE.set(JavaInterface {
            get_surface_width: env
                .get_method_id(class, "getSurfaceWidth", "()I")
                .expect("getSurfaceWidth must exist"),
            get_surface_height: env
                .get_method_id(class, "getSurfaceHeight", "()I")
                .expect("getSurfaceHeight must exist"),
            show_context_menu: env
                .get_method_id(class, "showContextMenu", "([Ljava/lang/String;)V")
                .expect("showContextMenu must exist"),
            get_trace_output: env
                .get_method_id(class, "getTraceOutput", "()Ljava/lang/String;")
                .expect("getTraceOutput must exist"),
            get_loc_in_window: env
                .get_method_id(class, "getLocInWindow", "()[I")
                .expect("getLocInWindow must exist"),
            get_android_data_storage_dir: env
                .get_method_id(class, "getAndroidDataStorageDir", "()Ljava/lang/String;")
                .expect("getAndroidDataStorageDir must exist"),
            get_android_app_data_dir: env
                .get_method_id(class, "getAndroidAppDataDir", "()Ljava/lang/String;")
                .expect("getAndroidAppDataDir must exist"),
            show_virtual_keyboard: env
                .get_method_id(class, "showVirtualKeyboard", "()V")
                .expect("showVirtualKeyboard must exist"),
            hide_virtual_keyboard: env
                .get_method_id(class, "hideVirtualKeyboard", "()V")
                .expect("hideVirtualKeyboard must exist"),
        });
    }
}
