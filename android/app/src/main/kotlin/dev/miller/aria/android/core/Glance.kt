package dev.miller.aria.android.core

import dev.miller.aria.android.link.LinkState
import dev.miller.aria.android.link.SessionInfo
import dev.miller.aria.android.mochi.BotState

/** One agent in a glance: its name and one plain line. No command, no path, no answer text. */
data class GlanceLine(val agent: String, val text: String, val state: BotState)

/** How loud a glance is: picks the dot colour of the widget, the tile state and the notification accent. */
enum class GlanceTone(val argb: Int) {
    CALM(0xFF8E8E93.toInt()),
    OK(0xFF34C759.toInt()),
    BUSY(0xFF0A84FF.toInt()),
    ATTENTION(0xFFFF9F0A.toInt()),
    TROUBLE(0xFFFF453A.toInt()),
}

/**
 * What the small surfaces show without opening the app: the ongoing notification, the home-screen widget and the
 * quick-settings tile. One pure summary so the three always say the same thing. Only agent names and the plain-language
 * lines of [HomeText] go in: never a command, a path, a file name or a final answer, because a widget or a tile can be
 * seen without unlocking the phone.
 */
data class Glance(
    val headline: String,
    val detail: String,
    val tone: GlanceTone,
    val connected: Boolean,
    val lines: List<GlanceLine>,
    /** Requests and questions waiting for the user. */
    val waiting: Int,
)

/**
 * The words of a glance. [ENGLISH] keeps this file pure for the JVM tests; the app passes the phone's language
 * (app/GlanceText.kt, from the glance_* strings). Templates use %1$s / %1$d like the string resources.
 */
data class GlanceWords(
    val notPaired: String = "Not paired",
    val notPairedDetail: String = "Open ARIA to connect to your computer",
    val connecting: String = "Connecting…",
    val notConnected: String = "Not connected",
    val reaching: String = "Reaching your computer",
    val cannotReach: String = "ARIA cannot reach your computer",
    val waitingForYou: String = "Waiting for you",
    val oneRequest: String = "1 request to approve",
    val manyRequests: String = "%1\$d requests to approve",
    val waitingAnswer: String = "Waiting for your answer",
    val oneQuestion: String = "1 question from an agent",
    val manyQuestions: String = "%1\$d questions from agents",
    val needsLook: String = "An agent needs a look",
    val oneStopped: String = "1 agent stopped",
    val manyStopped: String = "%1\$d agents stopped",
    val oneWorking: String = "%1\$s is working",
    val manyWorking: String = "%1\$d agents are working",
    val onDesktop: String = "On %1\$s",
    val working: String = "Working",
    val allDone: String = "All done",
    val finished: String = "Finished",
    val connected: String = "Connected",
    val toDesktop: String = "To %1\$s",
    val nothingRunning: String = "Nothing is running",
    val lineApproval: String = "Waiting for your approval",
    val lineQuestion: String = "Has a question",
    val lineError: String = "Stopped on an error",
    val lineLimit: String = "Hit a limit",
    val lineDizzy: String = "Needs a look",
    val lineIdle: String = "Idle",
    val lineThinking: String = "Thinking",
    val lineSearching: String = "Searching",
) {
    companion object {
        val ENGLISH = GlanceWords()
    }
}

object GlanceBuilder {
    const val MAX_LINES = 4
    private val BUSY = setOf(BotState.WORKING, BotState.THINKING, BotState.SEARCHING)
    private val TROUBLE = setOf(BotState.ERROR, BotState.RATELIMIT, BotState.DIZZY)

    /** Most urgent first; the same order as the Home list. */
    private val ORDER = listOf(
        BotState.APPROVAL, BotState.QUESTION, BotState.ERROR, BotState.RATELIMIT, BotState.DIZZY,
        BotState.WORKING, BotState.SEARCHING, BotState.THINKING, BotState.FINISHED, BotState.SLEEPING, BotState.IDLE,
    )

    private fun fill(template: String, value: Any): String = template.replace("%1\$s", value.toString()).replace("%1\$d", value.toString())

    fun build(
        paired: Boolean,
        link: LinkState,
        desktop: String?,
        sessions: List<SessionInfo>,
        approvals: Int,
        w: GlanceWords = GlanceWords.ENGLISH,
        nameOf: (String) -> String,
    ): Glance {
        if (!paired) return Glance(w.notPaired, w.notPairedDetail, GlanceTone.CALM, false, emptyList(), 0)
        if (link != LinkState.CONNECTED) {
            val connecting = link == LinkState.CONNECTING
            return Glance(
                if (connecting) w.connecting else w.notConnected,
                if (connecting) w.reaching else w.cannotReach,
                GlanceTone.CALM, false, emptyList(), 0,
            )
        }
        val ranked = sessions.sortedBy { ORDER.indexOf(it.state).let { i -> if (i < 0) ORDER.size else i } }
        val lines = ranked.take(MAX_LINES).map { GlanceLine(nameOf(it.pillId), lineOf(it, w), it.state) }
        val questions = sessions.count { it.state == BotState.QUESTION }
        val waiting = approvals + questions
        val busy = sessions.count { it.state in BUSY }
        val trouble = sessions.count { it.state in TROUBLE }
        val where = desktop?.takeIf { it.isNotBlank() }
        return when {
            approvals > 0 -> Glance(
                w.waitingForYou,
                if (approvals == 1) w.oneRequest else fill(w.manyRequests, approvals),
                GlanceTone.ATTENTION, true, lines, waiting,
            )
            questions > 0 -> Glance(
                w.waitingAnswer,
                if (questions == 1) w.oneQuestion else fill(w.manyQuestions, questions),
                GlanceTone.ATTENTION, true, lines, waiting,
            )
            trouble > 0 -> Glance(
                w.needsLook,
                if (trouble == 1) w.oneStopped else fill(w.manyStopped, trouble),
                GlanceTone.TROUBLE, true, lines, 0,
            )
            busy > 0 -> Glance(
                if (busy == 1) fill(w.oneWorking, nameOf(ranked.first { it.state in BUSY }.pillId)) else fill(w.manyWorking, busy),
                where?.let { fill(w.onDesktop, it) } ?: w.working,
                GlanceTone.BUSY, true, lines, 0,
            )
            sessions.any { it.state == BotState.FINISHED } -> Glance(
                w.allDone, where?.let { fill(w.onDesktop, it) } ?: w.finished, GlanceTone.OK, true, lines, 0,
            )
            else -> Glance(w.connected, where?.let { fill(w.toDesktop, it) } ?: w.nothingRunning, GlanceTone.OK, true, lines, 0)
        }
    }

    /** The plain line of a session: its state in a few words, never its step text or answer (those stay in the app). */
    private fun lineOf(s: SessionInfo, w: GlanceWords): String = when (s.state) {
        BotState.APPROVAL -> w.lineApproval
        BotState.QUESTION -> w.lineQuestion
        BotState.ERROR -> w.lineError
        BotState.RATELIMIT -> w.lineLimit
        BotState.DIZZY -> w.lineDizzy
        BotState.FINISHED -> w.finished
        BotState.SLEEPING, BotState.IDLE -> w.lineIdle
        BotState.THINKING -> w.lineThinking
        BotState.SEARCHING -> w.lineSearching
        else -> w.working // never the step text: it can hold a file name or a command
    }
}
