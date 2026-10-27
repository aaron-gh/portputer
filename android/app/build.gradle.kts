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
        // Portputer's version, and a build number that only goes up (the
        // commit count, as for the Mac and Windows apps), so each build
        // installs over the last and updates compare by it.
        val cargo = rootProject.file("../Cargo.toml").readText()
        val version = Regex("""(?m)^version = "(.*)"""").find(cargo)?.groupValues?.get(1) ?: "0.0.0"
        val commits = runCatching {
            providers.exec { commandLine("git", "rev-list", "--count", "HEAD") }.standardOutput.asText.get().trim().toInt()
        }.getOrNull()
        versionCode = System.getenv("PORTPUTER_BUILD_NUMBER")?.toIntOrNull() ?: commits ?: 1
        versionName = System.getenv("PORTPUTER_VERSION") ?: version
        ndk {
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
    }

    // Releases are signed with Portputer's own key (android/signing.sh), so
    // updates install over each other; without it, this computer's debug key.
    val portputerKey = file(System.getenv("PORTPUTER_ANDROID_KEYSTORE") ?: "${System.getProperty("user.home")}/.android/portputer.keystore")
    val portputerKeyPassword: String? = System.getenv("PORTPUTER_ANDROID_KEY_PASSWORD")
    signingConfigs {
        create("portputer") {
            storeFile = portputerKey
            storePassword = portputerKeyPassword
            keyAlias = "portputer"
            keyPassword = portputerKeyPassword
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = if (portputerKey.isFile && portputerKeyPassword != null) {
                signingConfigs.getByName("portputer")
            } else {
                signingConfigs.getByName("debug")
            }
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
