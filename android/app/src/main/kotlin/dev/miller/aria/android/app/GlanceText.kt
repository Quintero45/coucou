package dev.miller.aria.android.app

import android.content.Context
import dev.miller.aria.android.R
import dev.miller.aria.android.core.GlanceWords

/** The words of the widget, the tile and the ongoing notification, in the phone's language. */
object GlanceText {
    fun words(context: Context): GlanceWords {
        val s = { id: Int -> context.getString(id) }
        return GlanceWords(
            notPaired = s(R.string.glance_not_paired),
            notPairedDetail = s(R.string.glance_not_paired_detail),
            connecting = s(R.string.glance_connecting),
            notConnected = s(R.string.glance_not_connected),
            reaching = s(R.string.glance_reaching),
            cannotReach = s(R.string.glance_cannot_reach),
            waitingForYou = s(R.string.glance_waiting_for_you),
            oneRequest = s(R.string.glance_one_request),
            manyRequests = s(R.string.glance_many_requests),
            waitingAnswer = s(R.string.glance_waiting_answer),
            oneQuestion = s(R.string.glance_one_question),
            manyQuestions = s(R.string.glance_many_questions),
            needsLook = s(R.string.glance_needs_look),
            oneStopped = s(R.string.glance_one_stopped),
            manyStopped = s(R.string.glance_many_stopped),
            oneWorking = s(R.string.glance_one_working),
            manyWorking = s(R.string.glance_many_working),
            onDesktop = s(R.string.glance_on_desktop),
            working = s(R.string.glance_working),
            allDone = s(R.string.glance_all_done),
            finished = s(R.string.glance_finished),
            connected = s(R.string.glance_connected),
            toDesktop = s(R.string.glance_to_desktop),
            nothingRunning = s(R.string.glance_nothing_running),
            lineApproval = s(R.string.glance_line_approval),
            lineQuestion = s(R.string.glance_line_question),
            lineError = s(R.string.glance_line_error),
            lineLimit = s(R.string.glance_line_limit),
            lineDizzy = s(R.string.glance_line_dizzy),
            lineIdle = s(R.string.glance_line_idle),
            lineThinking = s(R.string.glance_line_thinking),
            lineSearching = s(R.string.glance_line_searching),
        )
    }
}
