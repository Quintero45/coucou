package dev.miller.aria.android

import java.io.File
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** The app speaks the desktop app's 10 languages, following the phone's language; English is the source. */
class LanguagesTest {
    private fun read(path: String) = File(path).readText()
    private val qualifiers = listOf("es", "fr", "pt-rBR", "ru", "in", "b+zh+Hans", "hi", "ar", "bn")

    @Test fun theBuildKeepsExactlyTheTenLanguages() {
        val gradle = read("build.gradle.kts")
        val kept = Regex("""resourceConfigurations\s*\+=\s*listOf\(([^)]*)\)""").find(gradle)!!.groupValues[1]
            .split(",").map { it.trim().trim('"') }
        assertEquals(listOf("en") + qualifiers, kept)
    }

    @Test fun thereIsNoLanguageChoiceAnywhere() {
        assertFalse(read("src/main/AndroidManifest.xml").contains("localeConfig"))
        val settings = read("src/main/kotlin/dev/miller/aria/android/ui/SettingsScreens.kt")
        assertFalse(settings.contains("LocaleManager"))
        assertFalse(settings.contains("applicationLocales"))
    }

    @Test fun englishStringsAreTheSingleSourceOfTruth() {
        val source = read("../i18n/app-strings.json")
        val english = Regex("""name="([a-z_0-9]+)"""").findAll(read("src/main/res/values/strings.xml")).map { it.groupValues[1] }.toSet()
        val declared = Regex("""^\s{4}"([a-z_0-9]+)":""", RegexOption.MULTILINE).findAll(source.substringAfter(""""strings"""")).map { it.groupValues[1] }.toSet()
        assertEquals(declared, english)
    }

    @Test fun everyLanguageHasEveryStringWithTheSamePlaceholders() {
        val folders = File("src/main/res").listFiles().orEmpty().map { it.name }.filter { it.startsWith("values") }
        assertEquals((listOf("values") + qualifiers.map { "values-$it" }).sorted(), folders.sorted())
        val en = JSONObject(read("../i18n/app-strings.json")).getJSONObject("strings")
        val ph = { s: String -> Regex("""%\d\$[sd]|%%""").findAll(s).map { it.value }.sorted().toList() }
        for (lang in listOf("es", "fr", "pt-BR", "ru", "id", "zh-Hans", "hi", "ar", "bn")) {
            val t = JSONObject(read("../i18n/app-strings.$lang.json")).getJSONObject("strings")
            for (k in en.keys()) {
                assertTrue("$lang lacks $k", t.has(k))
                assertEquals("$lang $k", ph(en.getString(k)), ph(t.getString(k)))
            }
        }
    }
}
