plugins {
  id("com.android.library")
  id("org.jetbrains.kotlin.android")
}

android {
  namespace = "dev.apell.sensors"
  compileSdk = 36
  defaultConfig { minSdk = 24 }
  compileOptions {
    sourceCompatibility = JavaVersion.VERSION_1_8
    targetCompatibility = JavaVersion.VERSION_1_8
  }
}

kotlin { compilerOptions { jvmTarget = org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_1_8 } }
dependencies {
  implementation(project(":tauri-android"))
  implementation("androidx.core:core:1.7.0")
  implementation("androidx.activity:activity:1.6.0")
  implementation("androidx.appcompat:appcompat:1.6.0")
}
