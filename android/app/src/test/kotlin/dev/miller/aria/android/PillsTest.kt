package dev.miller.aria.android

import dev.miller.aria.android.core.PillCategory
import dev.miller.aria.android.core.Pills
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Pill IDs are contract values: this fails if the catalog drifts from windows/src/core/pills.ts.
 * Coucou's Spotify pill stays on the phone (IDs are never removed) though ARIA's desktop has a music card instead.
 */
class PillsTest {
    private data class Ref(val id: String, val name: String, val color: String, val category: String)

    private fun reference(): List<Ref> {
        val src = ReferenceFiles.read("windows/src/core/pills.ts")
        val re = Regex("""\{\s*id:\s*"([^"]+)",\s*name:\s*"([^"]+)",\s*color:\s*([^,]+),\s*category:\s*"([^"]+)"""")
        val accents = Regex("""(\w+):\s*"(#[0-9A-Fa-f]{6})"""").findAll(src.substringBefore("export const PILL_CATALOG"))
            .associate { it.groupValues[1] to it.groupValues[2] }
        return re.findAll(src).map { m ->
            val raw = m.groupValues[3].trim()
            val color = if (raw.startsWith("\"")) raw.trim('"') else accents.getValue(raw.removePrefix("ACCENT."))
            Ref(m.groupValues[1], m.groupValues[2], color, m.groupValues[4])
        }.toList()
    }

    @Test fun catalogMatchesDesktopExceptAppleMusic() {
        val expected = reference().filter { it.id != "integration_music" }
        assertTrue("parsed too few pills from pills.ts", expected.size >= 20)
        val catalog = Pills.catalog.filter { it.id != "integration_spotify" }
        assertEquals(expected.map { it.id }, catalog.map { it.id })
        for ((ref, mine) in expected.zip(catalog)) {
            assertEquals(ref.name, mine.name)
            assertEquals(ref.color.uppercase(), mine.colorHex.uppercase())
            assertEquals(ref.category, mine.category.name.lowercase())
        }
    }

    @Test fun idsAreUniqueAndDefaultExists() {
        assertEquals(Pills.catalog.size, Pills.catalog.map { it.id }.toSet().size)
        assertEquals(PillCategory.WORKSPACE, Pills.byId(Pills.DEFAULT_MAIN_PILL)?.category)
    }
}
