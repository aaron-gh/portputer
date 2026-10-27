plugins {
    id("com.android.application")
}

android {
    namespace = "io.github.aaron_gh.portputer"
    compileSdk = 36

    defaultConfig {
        applicationId = "io.github.aaron_gh.portputer"
        // Android 8: AAudio, which the Rust core plays the computer's sound with.
        minSdk = 26
        targetSdk = 36
        val cargo = rootProject.file("../Cargo.toml").readText()
        val version = Regex("""(?m)^version = "(.*)"""").find(cargo)?.groupValues?.get(1) ?: "0.0.0"
        versionCode = System.getenv("PORTPUTER_BUILD_NUMBER")?.toIntOrNull() ?: 1
        versionName = System.getenv("PORTPUTER_VERSION") ?: version
        ndk {
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

// The Rust core and its Kotlin bindings, from build-rust.sh.
val buildRust by tasks.registering(Exec::class) {
    group = "build"
    description = "Builds the Rust core for Android and generates its Kotlin bindings."
    workingDir = rootProject.projectDir
    commandLine("./build-rust.sh")
    inputs.files(fileTree("../../crates") { include("**/*.rs", "**/Cargo.toml") })
    inputs.file("../../Cargo.lock")
    inputs.file("../build-rust.sh")
    outputs.dir("src/main/jniLibs")
    outputs.dir("src/main/java/uniffi")
}
tasks.named("preBuild") { dependsOn(buildRust) }

dependencies {
    implementation("net.java.dev.jna:jna:5.19.1@aar")
    implementation("androidx.core:core:1.17.0")
    implementation("androidx.recyclerview:recyclerview:1.4.0")
}
