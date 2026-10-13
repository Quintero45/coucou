package dev.miller.aria.android

import org.junit.Assert.assertEquals
import org.junit.Test

class AttributionTest {
    @Test fun nameAndRepoMatchPermission() {
        assertEquals("ARIA for Android", Attribution.APP_NAME)
        assertEquals("https://github.com/Louis-CFM/coucou", Attribution.REPO_URL)
    }
}
