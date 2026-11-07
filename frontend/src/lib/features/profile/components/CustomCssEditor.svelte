<!--
	The code field set up for a custom CSS sheet: the theme's variables and the
	style hooks offered while typing, and each refused part marked on its line
	with the same check the server runs. `limit` picks a person's or the site's
	length limit. `error` replaces the check's line under the editor, for a
	save the server refused.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import {
		customCssProblems,
		describeCssProblems,
		type CssLimit
	} from '$lib/stores/customCss.svelte';
	import rules from '$lib/stores/customCssRules.json';
	import CodeField from '$lib/ui/CodeField.svelte';
	import type { CodeProblem } from '$lib/ui/codeEditor';
	import { CSS_PLACEHOLDER, CSS_WORDS } from './cssVocabulary';

	interface Props {
		label?: string;
		value: string;
		limit?: CssLimit;
		help?: string;
		error?: string | null;
		readonly?: boolean;
		aside?: Snippet;
	}

	let {
		label = 'CSS rules',
		value = $bindable(''),
		limit = 'user',
		help,
		error = null,
		readonly = false,
		aside
	}: Props = $props();

	let field = $state<CodeField | null>(null);

	const problem = $derived(describeCssProblems(customCssProblems(value, limit), limit));

	function markers(text: string): CodeProblem[] {
		return customCssProblems(text, limit).map((found) => ({
			line: found.line,
			message: describeCssProblems([found], limit) ?? found.what
		}));
	}

	/** Put `text` at the cursor. */
	export function insert(text: string): void {
		field?.insert(text);
	}
</script>

<CodeField
	bind:this={field}
	bind:value
	{label}
	{help}
	{readonly}
	{aside}
	error={error ?? problem}
	limit={rules.limits[limit]}
	words={[...CSS_WORDS]}
	problems={markers}
	placeholder={readonly ? undefined : CSS_PLACEHOLDER}
/>
