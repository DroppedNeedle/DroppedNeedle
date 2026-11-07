// The CSS editor behind `CodeField`, loaded by a dynamic import so CodeMirror
// reaches only the pages that show one. The editor lives in a shadow root:
// CodeMirror's style modules then go in as a constructed sheet on that root,
// which the page's content-security policy does not govern, where on the
// document itself they would be a `<style>` element the policy refuses. The
// look comes only from the theme's variables, which reach into the root.
import {
	autocompletion,
	closeBrackets,
	closeBracketsKeymap,
	completionKeymap,
	type CompletionSource
} from '@codemirror/autocomplete';
import { defaultKeymap, history, historyKeymap, indentWithTab } from '@codemirror/commands';
import { css, cssLanguage } from '@codemirror/lang-css';
import { HighlightStyle, indentOnInput, syntaxHighlighting } from '@codemirror/language';
import { linter, lintGutter, type Diagnostic } from '@codemirror/lint';
import { Compartment, EditorState } from '@codemirror/state';
import {
	drawSelection,
	EditorView,
	highlightActiveLine,
	highlightActiveLineGutter,
	keymap,
	lineNumbers,
	placeholder as placeholderText
} from '@codemirror/view';
import { tags } from '@lezer/highlight';

/** A refused part of the text: its 1-based line and why. */
export interface CodeProblem {
	line: number;
	message: string;
}

/** A word the editor offers while typing. */
export interface CodeWord {
	label: string;
	/** What goes in, when it differs from the label. */
	apply?: string;
	detail?: string;
	type: 'variable' | 'property' | 'keyword';
}

export interface CodeEditorOptions {
	host: HTMLElement;
	text: string;
	label: string;
	readonly: boolean;
	placeholder?: string;
	words: () => CodeWord[];
	problems: (text: string) => CodeProblem[];
	onchange: (text: string) => void;
	onfocus: (focused: boolean) => void;
}

export interface CodeEditor {
	setText(text: string): void;
	setReadonly(readonly: boolean): void;
	insert(text: string): void;
	focus(): void;
	destroy(): void;
}

const theme = EditorView.theme({
	'&': {
		color: 'var(--color-base-content)',
		backgroundColor: 'var(--color-base-100)',
		border: '1px solid var(--dn-edge)',
		borderRadius: 'var(--radius-field)',
		fontSize: '0.875rem',
		minHeight: '16rem',
		maxHeight: '32rem'
	},
	'&.cm-focused': { outline: '2px solid var(--dn-focus)', outlineOffset: '2px' },
	'.cm-scroller': {
		fontFamily: 'var(--font-mono)',
		lineHeight: '1.6',
		overflow: 'auto'
	},
	'.cm-content': { caretColor: 'var(--color-base-content)', padding: '0.5rem 0' },
	'.cm-cursor, .cm-dropCursor': { borderLeftColor: 'var(--color-base-content)' },
	'&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, ::selection':
		{ backgroundColor: 'var(--dn-selected)' },
	'.cm-gutters': {
		backgroundColor: 'var(--color-base-200)',
		color: 'var(--dn-muted)',
		borderRight: '1px solid var(--dn-line)',
		borderRadius: 'var(--radius-field) 0 0 var(--radius-field)'
	},
	'.cm-activeLine': { backgroundColor: 'color-mix(in oklab, var(--dn-selected) 45%, transparent)' },
	'.cm-activeLineGutter': {
		backgroundColor: 'var(--dn-selected)',
		color: 'var(--color-base-content)'
	},
	'.cm-placeholder': { color: 'var(--dn-muted)' },
	'.cm-tooltip': {
		backgroundColor: 'var(--color-base-200)',
		color: 'var(--color-base-content)',
		border: '1px solid var(--dn-edge)',
		borderRadius: '0.5rem'
	},
	'.cm-tooltip-autocomplete > ul': { fontFamily: 'var(--font-mono)' },
	'.cm-tooltip-autocomplete > ul > li[aria-selected]': {
		backgroundColor: 'var(--dn-selected)',
		color: 'var(--color-base-content)'
	},
	'.cm-completionDetail': { color: 'var(--dn-muted)' },
	'.cm-diagnostic-error': { borderLeftColor: 'var(--color-error)' },
	'.cm-lintRange-error': {
		textDecoration: 'underline wavy var(--color-error)',
		textUnderlineOffset: '3px',
		backgroundImage: 'none'
	}
});

const highlight = HighlightStyle.define([
	{ tag: tags.comment, color: 'var(--dn-muted)', fontStyle: 'italic' },
	{ tag: [tags.tagName, tags.className, tags.labelName], color: 'var(--color-primary)' },
	{ tag: [tags.propertyName, tags.attributeName], color: 'var(--color-info)' },
	{ tag: [tags.variableName, tags.definition(tags.variableName)], color: 'var(--color-secondary)' },
	{ tag: [tags.string, tags.special(tags.string)], color: 'var(--color-success)' },
	{ tag: [tags.number, tags.unit, tags.color, tags.atom], color: 'var(--color-warning)' },
	{ tag: [tags.keyword, tags.modifier, tags.operatorKeyword], color: 'var(--color-accent)' },
	{ tag: tags.invalid, color: 'var(--color-error)' }
]);

// Typing over a selection that covers coloured text makes Chrome carry the
// colour into a `style` attribute on the new text, which the policy refuses.
// Such typing is applied here instead, so the browser never edits the page.
const replaceSelectionTyping = EditorView.domEventHandlers({
	beforeinput(event, view) {
		if (event.inputType !== 'insertText' || event.data === null) return false;
		if (view.state.readOnly || view.state.selection.ranges.every((range) => range.empty)) {
			return false;
		}
		event.preventDefault();
		view.dispatch(view.state.replaceSelection(event.data), {
			userEvent: 'input.type',
			scrollIntoView: true
		});
		return true;
	}
});

function wordsSource(words: () => CodeWord[]): CompletionSource {
	return (context) => {
		const before = context.matchBefore(/(\[[\w-]*(="?[\w-]*)?|--[\w-]*)$/);
		if (!before || (before.from === before.to && !context.explicit)) return null;
		const typed = before.text;
		const options = words()
			.filter((word) => (word.apply ?? word.label).startsWith(typed[0] ?? ''))
			.map((word) => ({
				label: word.label,
				apply: word.apply,
				detail: word.detail,
				type: word.type
			}));
		return options.length ? { from: before.from, options, validFor: /^[\w\-[="]*$/ } : null;
	};
}

export function createCodeEditor(options: CodeEditorOptions): CodeEditor {
	const root = options.host.shadowRoot ?? options.host.attachShadow({ mode: 'open' });
	const editable = new Compartment();
	let quiet = false;

	const lint = linter(
		(view): Diagnostic[] => {
			const doc = view.state.doc;
			return options.problems(doc.toString()).map((problem) => {
				const line = doc.line(Math.min(Math.max(problem.line, 1), doc.lines));
				return { from: line.from, to: line.to, severity: 'error', message: problem.message };
			});
		},
		{ delay: 300 }
	);

	const view = new EditorView({
		parent: root,
		root,
		state: EditorState.create({
			doc: options.text,
			extensions: [
				lineNumbers(),
				EditorView.lineWrapping,
				highlightActiveLineGutter(),
				highlightActiveLine(),
				drawSelection(),
				history(),
				indentOnInput(),
				closeBrackets(),
				css(),
				cssLanguage.data.of({ autocomplete: wordsSource(options.words) }),
				autocompletion(),
				syntaxHighlighting(highlight),
				lint,
				lintGutter(),
				keymap.of([
					...closeBracketsKeymap,
					...defaultKeymap,
					...historyKeymap,
					...completionKeymap,
					indentWithTab
				]),
				theme,
				options.placeholder ? placeholderText(options.placeholder) : [],
				EditorView.contentAttributes.of({
					'aria-label': options.label,
					'aria-multiline': 'true',
					spellcheck: 'false'
				}),
				editable.of(EditorState.readOnly.of(options.readonly)),
				replaceSelectionTyping,
				EditorView.updateListener.of((update) => {
					if (update.focusChanged) options.onfocus(update.view.hasFocus);
					if (update.docChanged && !quiet) options.onchange(update.state.doc.toString());
				})
			]
		})
	});

	return {
		setText(text) {
			if (text === view.state.doc.toString()) return;
			quiet = true;
			view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: text } });
			quiet = false;
		},
		setReadonly(readonly) {
			view.dispatch({ effects: editable.reconfigure(EditorState.readOnly.of(readonly)) });
		},
		insert(text) {
			if (view.state.readOnly) return;
			view.dispatch(view.state.replaceSelection(text), { scrollIntoView: true });
			view.focus();
		},
		focus() {
			view.focus();
		},
		destroy() {
			view.destroy();
		}
	};
}
