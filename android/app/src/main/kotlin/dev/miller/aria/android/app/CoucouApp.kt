package dev.miller.aria.android.app

import android.app.Application

class CoucouApp : Application() {
    val model: AppModel by lazy { AppModel(this) }
}
