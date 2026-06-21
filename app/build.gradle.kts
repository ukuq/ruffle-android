@file:Suppress("UnstableApiUsage")

import com.android.build.api.variant.FilterConfiguration.FilterType.ABI
import com.android.build.gradle.internal.cxx.configure.gradleLocalProperties
import com.github.willir.rust.CargoNdkBuildTask
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

val localProperties = gradleLocalProperties(rootDir, providers)
val abiFilterList = ((localProperties["ABI_FILTERS"] ?: properties["ABI_FILTERS"]) as? String)
    ?.split(';')
    ?.map { it.trim() }
    ?.filter { it.isNotEmpty() }
val ndkTargetList = ((localProperties["ndkTargets"] ?: properties["ndkTargets"]) as? String)
    ?.split(';')
    ?.map { it.trim() }
    ?.filter { it.isNotEmpty() }
val isGithubActions = System.getenv("GITHUB_ACTIONS") != null
val allAbiFilters = listOf("armeabi-v7a", "arm64-v8a", "x86", "x86_64")
val allNdkTargets = listOf(
    "armv7-linux-androideabi",
    "aarch64-linux-android",
    "i686-linux-android",
    "x86_64-linux-android"
)
val defaultAbiFilters = if (isGithubActions) allAbiFilters else listOf("arm64-v8a")
val defaultNdkTargets = if (isGithubActions) allNdkTargets else listOf("aarch64-linux-android")
val abiCodes = mapOf("armeabi-v7a" to 1, "arm64-v8a" to 2, "x86" to 3, "x86_64" to 4)

plugins {
    alias(libs.plugins.androidApplication)
    alias(libs.plugins.jetbrainsKotlinAndroid)
    alias(libs.plugins.cargoNdkAndroid)
}

android {
    namespace = "rs.ruffle"
    compileSdk = 36

    defaultConfig {
        applicationId = "rs.seer2"
        minSdk = 26
        targetSdk = 35
        versionCode = 1104
        versionName = "1.1.4"

        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        vectorDrawables {
            useSupportLibrary = true
        }
    }

    signingConfigs {
        val keyFile = System.getenv("SIGNING_STORE_FILE")
            ?.takeIf { it.isNotBlank() }
            ?.let { file(it) }
            ?: file("androidkey.jks")
        val storePasswordVal = System.getenv("SIGNING_STORE_PASSWORD")
        if (keyFile.exists() && storePasswordVal != null && storePasswordVal.isNotEmpty()) {
            create("release") {
                storeFile = keyFile
                storePassword = storePasswordVal
                keyAlias = System.getenv("SIGNING_KEY_ALIAS")
                keyPassword = System.getenv("SIGNING_KEY_PASSWORD") ?: storePasswordVal
            }
        }
    }

    buildTypes {
        debug {
            signingConfig = signingConfigs.findByName("release") ?: signingConfig
        }
        release {
            isMinifyEnabled = false
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )
            signingConfig = signingConfigs.findByName("release")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }

    buildFeatures {
        prefab = true
    }

    packaging {
        resources {
            excludes += "/META-INF/{AL2.0,LGPL2.1}"
        }
    }

    splits {
        // Configures multiple APKs based on ABI.
        abi {
            // Enables building multiple APKs per ABI.
            isEnable = true

            // Resets the list of ABIs that Gradle should create APKs for to none.
            reset()

            // Specifies a list of ABIs that Gradle should create APKs for.
            if (abiFilterList != null && abiFilterList.isNotEmpty()) {
                include(*abiFilterList.toTypedArray())
            } else {
                include(*defaultAbiFilters.toTypedArray())
            }
        }
    }
    dependenciesInfo {
        // Disables dependency metadata when building APKs.
        includeInApk = false
        // Disables dependency metadata when building Android App Bundles.
        includeInBundle = false
    }
}

kotlin {
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_1_8)
    }
}

androidComponents {
    onVariants { variant ->
        variant.outputs.forEach { output ->
            val name = output.filters.find { it.filterType == ABI }?.identifier
            val abiCode = abiCodes[name] ?: 0
            output.versionCode.set(output.versionCode.get() * 10 + abiCode)
        }
    }
}

dependencies {

    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.games.activity)
    implementation(libs.androidx.constraintlayout)
    implementation(libs.androidx.appcompat)
    androidTestImplementation(libs.androidx.uiautomator)
    androidTestImplementation(libs.androidx.test.runner)
    androidTestImplementation(libs.androidx.test.rules)
    testImplementation(libs.junit)
    androidTestImplementation(libs.androidx.junit)
    androidTestImplementation(libs.androidx.espresso.core)
}

// On GHA, we prebuild the native libs separately for fasterness,
// and this plugin doesn't recognize them, so would build them again.
if (System.getenv("GITHUB_ACTIONS") != null) {
    tasks.withType<CargoNdkBuildTask> {
        enabled = false
    }
}

cargoNdk {
    module = "."
    apiLevel = 26
    buildType = "release"

    if (!ndkTargetList.isNullOrEmpty()) {
        targets = ArrayList(ndkTargetList)
    } else {
        targets = ArrayList(defaultNdkTargets)
    }
}
