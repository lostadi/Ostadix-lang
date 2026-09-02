package org.ostadix.terminal;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.DialogInterface;
import android.graphics.Typeface;
import android.text.Spannable;
import android.text.SpannableString;
import android.text.style.ForegroundColorSpan;
import android.view.View;
import android.view.ViewGroup;
import android.widget.ArrayAdapter;
import android.widget.CompoundButton;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.SeekBar;
import android.widget.Spinner;
import android.widget.Switch;
import android.widget.TextView;

import java.util.Objects;

/** Programmatic, dependency-free terminal settings UI. */
public final class SettingsDialog {
    public interface Callback {
        void onSettingsApplied(AppPreferences.Snapshot settings);
    }

    private SettingsDialog() {
    }

    public static AlertDialog show(
            final Activity activity,
            final AppPreferences preferences,
            final Callback callback
    ) {
        Objects.requireNonNull(activity, "activity");
        Objects.requireNonNull(preferences, "preferences");

        final int outerPadding = dp(activity, 20);
        final int sectionSpacing = dp(activity, 18);
        final int rowSpacing = dp(activity, 10);

        final LinearLayout content = new LinearLayout(activity);
        content.setOrientation(LinearLayout.VERTICAL);
        content.setPadding(outerPadding, dp(activity, 8), outerPadding, outerPadding);

        TextView subtitle = text(activity, R.string.settings_subtitle, 14, false);
        subtitle.setTextColor(activity.getColor(R.color.ostadix_text_muted));
        content.addView(subtitle, matchWrap());

        TextView previewSection = section(activity, R.string.settings_preview_title);
        setTopMargin(content, previewSection, sectionSpacing);

        final TextView preview = new TextView(activity);
        preview.setTypeface(Typeface.MONOSPACE);
        preview.setLineSpacing(0f, 1.15f);
        preview.setPadding(dp(activity, 14), dp(activity, 12), dp(activity, 14), dp(activity, 12));
        setTopMargin(content, preview, dp(activity, 8));

        TextView appearanceSection = section(activity, R.string.settings_appearance_section);
        setTopMargin(content, appearanceSection, sectionSpacing);

        label(content, activity, R.string.setting_theme, rowSpacing);
        final Spinner theme = spinner(activity, R.array.theme_labels);
        content.addView(theme, matchWrap());

        final TextView fontValue = valueLabel(activity);
        addPairedLabel(content, activity, R.string.setting_font_size, fontValue, rowSpacing);
        final SeekBar fontSize = new SeekBar(activity);
        fontSize.setMax(AppPreferences.MAX_FONT_SIZE_SP - AppPreferences.MIN_FONT_SIZE_SP);
        content.addView(fontSize, matchWrap());

        final TextView scrollbackValue = valueLabel(activity);
        addPairedLabel(content, activity, R.string.setting_scrollback, scrollbackValue, rowSpacing);
        final SeekBar scrollback = new SeekBar(activity);
        scrollback.setMax(
                (AppPreferences.MAX_SCROLLBACK_LINES - AppPreferences.MIN_SCROLLBACK_LINES) / 500
        );
        content.addView(scrollback, matchWrap());

        label(content, activity, R.string.setting_cursor, rowSpacing);
        final Spinner cursor = spinner(activity, R.array.cursor_labels);
        content.addView(cursor, matchWrap());

        TextView behaviorSection = section(activity, R.string.settings_behavior_section);
        setTopMargin(content, behaviorSection, sectionSpacing);

        label(content, activity, R.string.setting_cpu_mode, rowSpacing);
        final Spinner cpuMode = spinner(activity, R.array.cpu_mode_labels);
        content.addView(cpuMode, matchWrap());

        label(content, activity, R.string.setting_startup, rowSpacing);
        final Spinner startup = spinner(activity, R.array.startup_mode_labels);
        content.addView(startup, matchWrap());

        final Switch keepAwake = settingSwitch(
                activity,
                R.string.setting_keep_awake,
                R.string.setting_keep_awake_summary
        );
        setTopMargin(content, keepAwake, rowSpacing);

        final Switch haptics = settingSwitch(
                activity,
                R.string.setting_haptics,
                R.string.setting_haptics_summary
        );
        setTopMargin(content, haptics, dp(activity, 4));

        final String[] themeValues = AppPreferences.themeValues();
        final String[] cursorValues = AppPreferences.cursorStyleValues();
        final String[] cpuValues = AppPreferences.cpuModeValues();
        final String[] startupValues = AppPreferences.startupModeValues();

        final Runnable refreshPreview = new Runnable() {
            @Override
            public void run() {
                int fontSp = AppPreferences.MIN_FONT_SIZE_SP + fontSize.getProgress();
                String selectedTheme = themeValues[theme.getSelectedItemPosition()];
                String selectedCursor = cursorValues[cursor.getSelectedItemPosition()];
                AppPreferences.ThemePalette palette = AppPreferences.paletteFor(selectedTheme);

                String cursorGlyph;
                if (AppPreferences.CURSOR_BAR.equals(selectedCursor)) {
                    cursorGlyph = "\u2502";
                } else if (AppPreferences.CURSOR_UNDERLINE.equals(selectedCursor)) {
                    cursorGlyph = "\u2581";
                } else {
                    cursorGlyph = "\u2588";
                }

                String sample = activity.getString(R.string.terminal_preview, cursorGlyph);
                SpannableString styled = new SpannableString(sample);
                int cursorStart = sample.length() - cursorGlyph.length();
                styled.setSpan(
                        new ForegroundColorSpan(palette.cursor),
                        cursorStart,
                        sample.length(),
                        Spannable.SPAN_EXCLUSIVE_EXCLUSIVE
                );
                preview.setBackgroundColor(palette.background);
                preview.setTextColor(palette.foreground);
                preview.setTextSize(fontSp);
                preview.setText(styled);
                fontValue.setText(activity.getString(R.string.setting_font_size_value, fontSp));
            }
        };

        fontSize.setOnSeekBarChangeListener(new SimpleSeekListener() {
            @Override
            public void onProgressChanged(SeekBar seekBar, int progress, boolean fromUser) {
                refreshPreview.run();
            }
        });
        scrollback.setOnSeekBarChangeListener(new SimpleSeekListener() {
            @Override
            public void onProgressChanged(SeekBar seekBar, int progress, boolean fromUser) {
                int lines = AppPreferences.MIN_SCROLLBACK_LINES + progress * 500;
                scrollbackValue.setText(
                        activity.getString(R.string.setting_scrollback_value, lines)
                );
            }
        });
        theme.setOnItemSelectedListener(new SimpleItemSelectedListener(refreshPreview));
        cursor.setOnItemSelectedListener(new SimpleItemSelectedListener(refreshPreview));

        final Runnable loadCurrent = new Runnable() {
            @Override
            public void run() {
                bind(
                        preferences.snapshot(),
                        theme,
                        themeValues,
                        fontSize,
                        scrollback,
                        cursor,
                        cursorValues,
                        cpuMode,
                        cpuValues,
                        keepAwake,
                        haptics,
                        startup,
                        startupValues
                );
                refreshPreview.run();
                int lines = AppPreferences.MIN_SCROLLBACK_LINES + scrollback.getProgress() * 500;
                scrollbackValue.setText(
                        activity.getString(R.string.setting_scrollback_value, lines)
                );
            }
        };
        loadCurrent.run();

        ScrollView scrollView = new ScrollView(activity);
        scrollView.setFillViewport(true);
        scrollView.addView(content, new ScrollView.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT
        ));

        final AlertDialog dialog = new AlertDialog.Builder(activity)
                .setTitle(R.string.settings_title)
                .setView(scrollView)
                .setPositiveButton(R.string.settings_apply, null)
                .setNegativeButton(R.string.settings_cancel, null)
                .setNeutralButton(R.string.settings_reset, null)
                .create();

        dialog.setOnShowListener(new DialogInterface.OnShowListener() {
            @Override
            public void onShow(DialogInterface ignored) {
                dialog.getButton(AlertDialog.BUTTON_POSITIVE).setOnClickListener(new View.OnClickListener() {
                    @Override
                    public void onClick(View view) {
                        AppPreferences.Snapshot updated = new AppPreferences.Snapshot(
                                themeValues[theme.getSelectedItemPosition()],
                                AppPreferences.MIN_FONT_SIZE_SP + fontSize.getProgress(),
                                AppPreferences.MIN_SCROLLBACK_LINES + scrollback.getProgress() * 500,
                                cursorValues[cursor.getSelectedItemPosition()],
                                cpuValues[cpuMode.getSelectedItemPosition()],
                                keepAwake.isChecked(),
                                haptics.isChecked(),
                                startupValues[startup.getSelectedItemPosition()]
                        );
                        preferences.apply(updated);
                        AppPreferences.Snapshot applied = preferences.snapshot();
                        if (callback != null) {
                            callback.onSettingsApplied(applied);
                        }
                        dialog.dismiss();
                    }
                });
                dialog.getButton(AlertDialog.BUTTON_NEUTRAL).setOnClickListener(new View.OnClickListener() {
                    @Override
                    public void onClick(View view) {
                        bind(
                                AppPreferences.defaults(),
                                theme,
                                themeValues,
                                fontSize,
                                scrollback,
                                cursor,
                                cursorValues,
                                cpuMode,
                                cpuValues,
                                keepAwake,
                                haptics,
                                startup,
                                startupValues
                        );
                        refreshPreview.run();
                        int lines = AppPreferences.MIN_SCROLLBACK_LINES
                                + scrollback.getProgress() * 500;
                        scrollbackValue.setText(
                                activity.getString(R.string.setting_scrollback_value, lines)
                        );
                    }
                });
            }
        });
        dialog.show();
        return dialog;
    }

    private static void bind(
            AppPreferences.Snapshot settings,
            Spinner theme,
            String[] themeValues,
            SeekBar fontSize,
            SeekBar scrollback,
            Spinner cursor,
            String[] cursorValues,
            Spinner cpuMode,
            String[] cpuValues,
            CompoundButton keepAwake,
            CompoundButton haptics,
            Spinner startup,
            String[] startupValues
    ) {
        theme.setSelection(indexOf(themeValues, settings.theme));
        fontSize.setProgress(settings.fontSizeSp - AppPreferences.MIN_FONT_SIZE_SP);
        scrollback.setProgress(
                (settings.scrollbackLines - AppPreferences.MIN_SCROLLBACK_LINES + 250) / 500
        );
        cursor.setSelection(indexOf(cursorValues, settings.cursorStyle));
        cpuMode.setSelection(indexOf(cpuValues, settings.cpuMode));
        keepAwake.setChecked(settings.keepScreenAwake);
        haptics.setChecked(settings.hapticsEnabled);
        startup.setSelection(indexOf(startupValues, settings.startupMode));
    }

    private static Spinner spinner(Activity activity, int arrayResource) {
        Spinner spinner = new Spinner(activity, Spinner.MODE_DROPDOWN);
        ArrayAdapter<CharSequence> adapter = ArrayAdapter.createFromResource(
                activity,
                arrayResource,
                android.R.layout.simple_spinner_item
        );
        adapter.setDropDownViewResource(android.R.layout.simple_spinner_dropdown_item);
        spinner.setAdapter(adapter);
        return spinner;
    }

    private static Switch settingSwitch(
            Activity activity,
            int labelResource,
            int summaryResource
    ) {
        Switch toggle = new Switch(activity);
        toggle.setText(labelResource);
        toggle.setContentDescription(
                activity.getString(labelResource) + ". " + activity.getString(summaryResource)
        );
        toggle.setPadding(0, dp(activity, 6), 0, dp(activity, 6));
        return toggle;
    }

    private static TextView section(Activity activity, int textResource) {
        TextView view = text(activity, textResource, 14, true);
        view.setTextColor(activity.getColor(R.color.ostadix_accent));
        return view;
    }

    private static void label(
            LinearLayout content,
            Activity activity,
            int textResource,
            int topMargin
    ) {
        TextView label = text(activity, textResource, 14, true);
        setTopMargin(content, label, topMargin);
    }

    private static void addPairedLabel(
            LinearLayout content,
            Activity activity,
            int textResource,
            TextView value,
            int topMargin
    ) {
        LinearLayout row = new LinearLayout(activity);
        row.setOrientation(LinearLayout.HORIZONTAL);
        TextView label = text(activity, textResource, 14, true);
        row.addView(label, new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f));
        row.addView(value, wrapWrap());
        setTopMargin(content, row, topMargin);
    }

    private static TextView valueLabel(Activity activity) {
        TextView value = new TextView(activity);
        value.setTextSize(13);
        value.setTextColor(activity.getColor(R.color.ostadix_text_muted));
        return value;
    }

    private static TextView text(
            Activity activity,
            int textResource,
            int textSizeSp,
            boolean bold
    ) {
        TextView view = new TextView(activity);
        view.setText(textResource);
        view.setTextSize(textSizeSp);
        view.setTextColor(activity.getColor(R.color.ostadix_text));
        if (bold) {
            view.setTypeface(Typeface.DEFAULT, Typeface.BOLD);
        }
        return view;
    }

    private static void setTopMargin(LinearLayout content, View child, int margin) {
        LinearLayout.LayoutParams params = matchWrap();
        params.topMargin = margin;
        content.addView(child, params);
    }

    private static LinearLayout.LayoutParams matchWrap() {
        return new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT
        );
    }

    private static LinearLayout.LayoutParams wrapWrap() {
        return new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT,
                ViewGroup.LayoutParams.WRAP_CONTENT
        );
    }

    private static int indexOf(String[] values, String target) {
        for (int i = 0; i < values.length; i++) {
            if (values[i].equals(target)) {
                return i;
            }
        }
        return 0;
    }

    private static int dp(Activity activity, int value) {
        return Math.round(value * activity.getResources().getDisplayMetrics().density);
    }

    private abstract static class SimpleSeekListener implements SeekBar.OnSeekBarChangeListener {
        @Override
        public void onStartTrackingTouch(SeekBar seekBar) {
        }

        @Override
        public void onStopTrackingTouch(SeekBar seekBar) {
        }
    }

    private static final class SimpleItemSelectedListener
            implements android.widget.AdapterView.OnItemSelectedListener {
        private final Runnable action;

        private SimpleItemSelectedListener(Runnable action) {
            this.action = action;
        }

        @Override
        public void onItemSelected(
                android.widget.AdapterView<?> parent,
                View view,
                int position,
                long id
        ) {
            action.run();
        }

        @Override
        public void onNothingSelected(android.widget.AdapterView<?> parent) {
        }
    }
}
