# The plugin class and its argument types are kept by tauri-android's own
# consumer rules (@TauriPlugin, @InvokeArg); the service is kept through the
# manifest.
# The background check's JNI entry point: Rust exports it by name
# (Java_dev_tessera_background_Native_pullNative).
-keepclasseswithmembernames class dev.tessera.background.Native {
    native <methods>;
}
