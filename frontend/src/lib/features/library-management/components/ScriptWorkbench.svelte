<!--
	A naming or tagging script's editor: the source, what it does on three
	example albums as it is typed (the part the last change altered
	highlighted for a moment), the server's complaint with a fix where a
	name is misspelt, and blocks that put a value or a ready-made piece at
	the cursor.
-->
<script lang="ts">
	import { tick, untrack } from 'svelte';
	import { CircleAlert, TriangleAlert, Undo2 } from 'lucide-svelte';

	import { getScriptPreviewQuery } from '$lib/features/library-management/queries/LibraryManagementQueries.svelte';
	import type {
		ExampleAlbum,
		ScriptPreviewFault,
		ScriptPreviewKind,
		ScriptPreviewSample
	} from '$lib/features/library-management/queries/types';
	import Button from '$lib/ui/Button.svelte';
	import TextAreaField from '$lib/ui/TextAreaField.svelte';
	import { outsideLink } from '$lib/ui/outsideLink';

	import {
		NAMING_BLOCKS,
		TAGGING_BLOCKS,
		changedSpan,
		replaceName,
		type ScriptBlock
	} from './scriptBlocks';

	interface Props {
		kind: ScriptPreviewKind;
		label: string;
		value: string;
		help?: string;
		error?: string | null;
		rows?: number;
		/** Called with the source after each change. */
		oninput?: (source: string) => void;
	}

	let { kind, label, value = $bindable(''), help, error, rows = 2, oninput }: Props = $props();

	const REFERENCE =
		'https://github.com/DroppedNeedle/DroppedNeedle/blob/v3/docs/library-management.md';
	const SAMPLES: { id: ExampleAlbum; label: string }[] = [
		{ id: 'one_disc', label: 'One disc' },
		{ id: 'two_discs', label: 'Two discs' },
		{ id: 'name_shared', label: 'Name shared' }
	];
	/** How long the part a change altered stays highlighted. */
	const HIGHLIGHT_MS = 1200;
	/** How long an answer may take before "Checking..." shows. */
	const CHECKING_AFTER_MS = 600;

	const tagging = $derived(kind === 'tagging');
	const groups = $derived(tagging ? TAGGING_BLOCKS : NAMING_BLOCKS);
	const anchor = $derived(tagging ? '#tagging-scripts' : '#naming-and-tagging-scripts');

	let element = $state<HTMLTextAreaElement | null>(null);
	let sample = $state<ExampleAlbum>('one_disc');
	let history = $state<string[]>([]);

	const preview = getScriptPreviewQuery(
		() => kind,
		() => value
	);

	// What each example last showed without a fault, kept so a script
	// being fixed still shows where it was, and to compare the next with.
	let lastGood = $state<Partial<Record<ExampleAlbum, ScriptPreviewSample>>>({});
	let marks = $state<Partial<Record<ExampleAlbum, { start: number; end: number }>>>({});
	let changedTags = $state<Partial<Record<ExampleAlbum, string[]>>>({});
	let clearTimer: ReturnType<typeof setTimeout> | undefined;

	const tagSignature = (entry: ScriptPreviewSample, name: string) =>
		JSON.stringify(entry.tags.find((change) => change.name === name) ?? null);

	function absorb(samples: ScriptPreviewSample[], fault: ScriptPreviewFault | null): void {
		if (fault) return;
		const nextMarks: typeof marks = {};
		const nextTags: typeof changedTags = {};
		const good = { ...lastGood };
		for (const entry of samples) {
			if (entry.fault) continue;
			const before = good[entry.sample];
			if (before && entry.path && before.path) {
				const span = changedSpan(before.path, entry.path);
				if (span) nextMarks[entry.sample] = span;
			}
			if (before && tagging) {
				nextTags[entry.sample] = entry.tags
					.map((change) => change.name)
					.filter((name) => tagSignature(entry, name) !== tagSignature(before, name));
			}
			good[entry.sample] = entry;
		}
		lastGood = good;
		marks = nextMarks;
		changedTags = nextTags;
		clearTimeout(clearTimer);
		clearTimer = setTimeout(() => {
			marks = {};
			changedTags = {};
		}, HIGHLIGHT_MS);
	}

	$effect(() => {
		const data = preview.data;
		if (!data || preview.isPlaceholderData) return;
		untrack(() => absorb(data.samples, data.fault ?? null));
	});
	$effect(() => () => clearTimeout(clearTimer));

	let slow = $state(false);
	$effect(() => {
		if (!preview.isFetching) {
			slow = false;
			return;
		}
		const timer = setTimeout(() => (slow = true), CHECKING_AFTER_MS);
		return () => clearTimeout(timer);
	});

	// An empty box shows no example, not the last one it had.
	const blank = $derived(value.trim() === '');
	const latest = $derived(preview.data?.samples.find((entry) => entry.sample === sample) ?? null);
	const values = $derived(latest?.values ?? {});
	const topFault = $derived(blank ? null : (preview.data?.fault ?? null));
	const current = $derived(blank ? null : latest);
	const sampleFault = $derived(topFault ? null : (current?.fault ?? null));
	const emptyParts = $derived(topFault ? null : (current?.empty_name_parts ?? null));
	const shown = $derived(
		blank ? null : !topFault && current && !current.fault ? current : (lastGood[sample] ?? null)
	);
	const stale = $derived(Boolean(topFault || sampleFault) || slow);
	const sampleLabel = $derived(SAMPLES.find((entry) => entry.id === sample)?.label ?? '');
	const mark = $derived(stale ? null : (marks[sample] ?? null));

	function setSource(next: string): void {
		value = next;
		oninput?.(next);
	}

	// Undo takes back blocks and fixes only: typing ends what it can undo,
	// so it never throws away what was typed since.
	function type(next: string): void {
		history = [];
		setSource(next);
	}

	async function put(text: string, ownLine = false): Promise<void> {
		const box = element;
		const start = box?.selectionStart ?? value.length;
		const end = box?.selectionEnd ?? value.length;
		let insert = text;
		if (ownLine) {
			if (start > 0 && value[start - 1] !== '\n') insert = `\n${insert}`;
			if (end < value.length && value[end] !== '\n') insert = `${insert}\n`;
		}
		history = [...history, value];
		setSource(value.slice(0, start) + insert + value.slice(end));
		await tick();
		if (!box) return;
		const from = start + insert.indexOf(text);
		box.focus();
		box.setSelectionRange(from, from + text.length);
	}

	function undo(): void {
		const previous = history.at(-1);
		if (previous === undefined) return;
		history = history.slice(0, -1);
		setSource(previous);
	}

	function useSuggestion(fault: ScriptPreviewFault): void {
		const suggestion = fault.suggestion;
		if (!suggestion) return;
		const fixed = replaceName(value, fault.line, fault.column, suggestion.found, suggestion.name);
		if (fixed === null) return;
		history = [...history, value];
		setSource(fixed);
	}

	function blockSample(block: ScriptBlock): string | null {
		if (!latest) return null;
		const text = block.sample(values);
		if (text === null) return null;
		return text.trim() === '' ? 'empty here' : text.trim();
	}

	function emptyWarning(parts: string[]): string {
		const fileName = parts.at(-1) === '';
		const what = fileName ? 'the file name' : 'a folder name';
		const result =
			kind === 'import_naming' ? "that album wouldn't import" : 'those files stay where they are';
		return `On "${sampleLabel}" ${what} comes out empty, so ${result}.`;
	}
</script>

<div class="script-workbench">
	<TextAreaField {label} {help} {error} {rows} mono bind:element bind:value={() => value, type}>
		{#snippet aside()}
			<a
				class="link text-sm"
				href={outsideLink(`${REFERENCE}${anchor}`)}
				target="_blank"
				rel="noopener noreferrer">Script reference</a
			>
		{/snippet}
	</TextAreaField>

	{#if topFault}
		<div class="script-panel" role="alert">
			<p class="flex items-center gap-2 text-sm font-semibold text-error">
				<CircleAlert class="size-4 shrink-0" aria-hidden="true" />
				{#if topFault.suggestion}
					"{topFault.suggestion.found}" isn't a value. Did you mean "{topFault.suggestion.name}"?
				{:else}
					{topFault.message}
				{/if}
			</p>
			<div class="flex flex-wrap items-center gap-3">
				{#if topFault.suggestion}
					<Button variant="primary" size="sm" onclick={() => useSuggestion(topFault)}
						>Use "{topFault.suggestion.name}"</Button
					>
				{/if}
				<span class="text-xs text-muted">Line {topFault.line}, column {topFault.column}.</span>
			</div>
		</div>
	{/if}

	<div class="script-panel" aria-live="polite">
		<div class="flex flex-wrap items-center justify-between gap-2">
			<p class="script-caption">
				<span class={['script-dot', stale && 'script-dot-stale']}></span>
				{#if slow}
					Checking...
				{:else if tagging}
					What changes on the example track
				{:else}
					Files land here
				{/if}
			</p>
			<div class="script-samples" role="group" aria-label="Example album">
				{#each SAMPLES as entry (entry.id)}
					<Button
						variant={sample === entry.id ? 'primary' : 'ghost'}
						size="sm"
						pill
						active={sample === entry.id}
						aria-pressed={sample === entry.id}
						onclick={() => (sample = entry.id)}>{entry.label}</Button
					>
				{/each}
			</div>
		</div>

		{#if blank}
			<p class="text-sm text-muted">Write a script to see an example.</p>
		{:else if emptyParts}
			<p class="flex items-center gap-2 text-sm font-semibold text-warning">
				<TriangleAlert class="size-4 shrink-0" aria-hidden="true" />
				{emptyWarning(emptyParts)}
			</p>
			<p class="script-path">
				<span class="text-muted">/music/</span
				>{#each emptyParts as part, index (index)}{#if index > 0}<span class="text-muted">/</span
						>{/if}{#if part === ''}<span class="script-empty">empty</span>{:else}{part}{/if}{/each}
			</p>
		{:else if sampleFault}
			<p class="flex items-center gap-2 text-sm font-semibold text-error" role="alert">
				<CircleAlert class="size-4 shrink-0" aria-hidden="true" />
				On "{sampleLabel}": {sampleFault.message}
			</p>
		{/if}

		{#if !emptyParts}
			{#if tagging}
				{#if shown && shown.tags.length > 0}
					<table class={['script-tags', stale && 'script-stale']}>
						<thead>
							<tr><th scope="col">Tag</th><th scope="col">Before</th><th scope="col">After</th></tr>
						</thead>
						<tbody>
							{#each shown.tags as change (change.name)}
								<tr
									class={[!stale && changedTags[sample]?.includes(change.name) && 'script-changed']}
								>
									<td class="font-mono">{change.name}</td>
									<td class="text-muted">{change.before.join('; ') || '-'}</td>
									<td>
										{#if change.after.length === 0}
											<span class="text-error">removed</span>
										{:else}
											{change.after.join('; ')}
										{/if}
									</td>
								</tr>
							{/each}
						</tbody>
					</table>
				{:else if shown}
					<p class={['text-sm text-muted', stale && 'script-stale']}>
						Nothing changes on this track.
					</p>
				{/if}
			{:else if shown?.path}
				<p class={['script-path', stale && 'script-stale']}>
					<span class="text-muted">/music/</span>{#if mark}{shown.path.slice(0, mark.start)}<mark
							class="script-mark">{shown.path.slice(mark.start, mark.end)}</mark
						>{shown.path.slice(mark.end)}{:else}{shown.path}{/if}
				</p>
			{/if}
		{/if}
	</div>

	<div class="space-y-3">
		<div class="flex flex-wrap items-center justify-between gap-2">
			<p class="text-sm font-semibold">
				Add a block <span class="font-normal text-muted">at the cursor</span>
			</p>
			{#if history.length > 0}
				<Button variant="outline" size="sm" icon={Undo2} onclick={undo}>Undo</Button>
			{/if}
		</div>
		{#each groups as group (group.name)}
			<div class="script-group">
				<p class="script-group-name">{group.name}</p>
				<div class="flex flex-wrap gap-2">
					{#each group.blocks as block (block.label)}
						{@const shownValue = blockSample(block)}
						<Button
							variant={block.look === 'value' ? 'outline' : 'neutral'}
							wrap
							title={block.code}
							onclick={() => put(block.code, block.ownLine)}
						>
							<span class={['font-semibold', block.look === 'between' && 'font-mono']}
								>{block.label}</span
							>
							{#if shownValue !== null}
								<span class="font-normal text-muted">{shownValue}</span>
							{/if}
						</Button>
					{/each}
				</div>
			</div>
		{/each}
	</div>
</div>

<style>
	.script-workbench {
		display: grid;
		gap: 1rem;
	}

	.script-panel {
		display: grid;
		gap: 0.6rem;
		padding: 0.9rem 1rem;
		border-radius: var(--radius-box);
		background: var(--color-base-300);
	}

	.script-samples {
		display: flex;
		gap: 0.25rem;
		padding: 0.2rem;
		border-radius: 999px;
		background: var(--color-base-100);
	}

	.script-caption {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		font-size: 0.75rem;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--color-muted);
	}

	.script-dot {
		width: 0.5rem;
		height: 0.5rem;
		border-radius: 999px;
		background: var(--color-primary);
	}

	.script-dot-stale {
		background: var(--color-muted);
	}

	.script-path {
		font-family: var(--font-mono);
		font-size: 1rem;
		line-height: 1.6;
		overflow-wrap: anywhere;
	}

	.script-stale {
		opacity: 0.55;
	}

	.script-mark {
		border-radius: 0.25rem;
		color: inherit;
		background: color-mix(in oklab, var(--color-primary) 22%, transparent);
		box-shadow: 0 0 0 1px var(--color-primary);
	}

	/* The timer takes the mark away; with motion allowed it fades first. */
	@media (prefers-reduced-motion: no-preference) {
		.script-mark {
			animation: script-mark-fade 1.2s ease-in forwards;
		}
	}

	@keyframes script-mark-fade {
		60% {
			background: color-mix(in oklab, var(--color-primary) 22%, transparent);
			box-shadow: 0 0 0 1px var(--color-primary);
		}
		100% {
			background: transparent;
			box-shadow: 0 0 0 1px transparent;
		}
	}

	.script-empty {
		margin: 0 0.15rem;
		padding: 0 0.35rem;
		border-radius: 0.25rem;
		font-family: var(--font-sans);
		font-size: 0.75rem;
		color: var(--color-warning);
		box-shadow: 0 0 0 1px var(--color-warning);
	}

	.script-tags {
		width: 100%;
		font-size: 0.875rem;
		border-collapse: collapse;
	}

	.script-tags th {
		padding: 0.4rem 0.5rem 0.4rem 0;
		font-size: 0.75rem;
		font-weight: 400;
		text-align: left;
		color: var(--color-muted);
	}

	.script-tags td {
		padding: 0.5rem 0.5rem 0.5rem 0;
		border-top: 1px solid var(--color-line);
		overflow-wrap: anywhere;
		vertical-align: top;
	}

	.script-tags tr {
		transition: background-color 0.6s ease-out;
	}

	.script-changed {
		background: color-mix(in oklab, var(--color-primary) 14%, transparent);
	}

	.script-group {
		display: grid;
		grid-template-columns: 7rem minmax(0, 1fr);
		gap: 0.75rem;
		align-items: start;
	}

	.script-group-name {
		padding-top: 0.6rem;
		font-size: 0.75rem;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--color-muted);
	}

	@media (max-width: 639px) {
		.script-group {
			grid-template-columns: minmax(0, 1fr);
			gap: 0.35rem;
		}

		.script-group-name {
			padding-top: 0;
		}

		.script-samples {
			width: 100%;
		}

		.script-samples > :global(*) {
			flex: 1 1 0;
			padding-inline: 0.5rem;
		}
	}
</style>
