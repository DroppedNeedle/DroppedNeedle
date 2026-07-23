<script lang="ts">
	import { ArrowDown, ArrowUp, Copy, FileCode2, Plus } from 'lucide-svelte';

	import type { ManagementScriptSettings } from '$lib/queries/library-management/types';
	import { createUuid } from '$lib/utils/uuid';

	interface Props {
		kind: 'naming' | 'tagging';
		scripts: ManagementScriptSettings[];
		selectedIds: string[];
		onchange: (scripts: ManagementScriptSettings[], selectedIds: string[]) => void;
	}

	let { kind, scripts, selectedIds, onchange }: Props = $props();
	const initialEditorId = (): string => selectedIds[0] ?? scripts[0]?.id ?? '';
	let editorScriptId = $state(initialEditorId());

	const selectedScript = $derived(scripts.find((script) => script.id === editorScriptId) ?? null);
	const attached = $derived(selectedIds.includes(editorScriptId));
	const localProblem = $derived(validateLocally(selectedScript));

	function validateLocally(script: ManagementScriptSettings | null): string {
		if (!script) return 'Choose or create a script.';
		if (!script.name.trim()) return 'The script needs a name.';
		if (!script.source.trim()) return 'The script needs source text.';
		if (kind === 'naming') {
			if (script.source.includes('\n') || script.source.includes('\r')) {
				return 'Naming scripts must be one path-template line.';
			}
			let depth = 0;
			for (const character of script.source) {
				if (character === '{') depth += 1;
				if (character === '}') depth -= 1;
				if (depth < 0) return 'A closing brace has no matching opening brace.';
			}
			if (depth !== 0) return 'An expression brace is not closed.';
		}
		return '';
	}

	function updateScript(update: Partial<Pick<ManagementScriptSettings, 'name' | 'source'>>): void {
		if (!selectedScript) return;
		onchange(
			scripts.map((script) =>
				script.id === selectedScript.id ? { ...script, ...update } : script
			),
			selectedIds
		);
	}

	function addScript(copySource = false): void {
		const source =
			copySource && selectedScript
				? selectedScript.source
				: kind === 'naming'
					? '{albumartist}/{album} ({year})/{disc:02}-{track:02} {title}.{ext}'
					: 'set title = title(title)';
		const script: ManagementScriptSettings = {
			id: createUuid(),
			name: copySource && selectedScript ? `${selectedScript.name} copy` : `New ${kind} script`,
			source,
			revision: '',
			preset_origin: null,
			preset_version: null
		};
		editorScriptId = script.id;
		onchange([...scripts, script], kind === 'naming' ? [script.id] : selectedIds);
	}

	function toggleAttached(): void {
		if (!selectedScript) return;
		if (kind === 'naming') {
			onchange(scripts, [selectedScript.id]);
			return;
		}
		onchange(
			scripts,
			attached
				? selectedIds.filter((id) => id !== selectedScript.id)
				: [...selectedIds, selectedScript.id]
		);
	}

	function moveAttached(direction: -1 | 1): void {
		const index = selectedIds.indexOf(editorScriptId);
		const target = index + direction;
		if (index < 0 || target < 0 || target >= selectedIds.length) return;
		const next = [...selectedIds];
		[next[index], next[target]] = [next[target], next[index]];
		onchange(scripts, next);
	}
</script>

<div class="management-script-editor">
	<div class="management-script-toolbar">
		<label class="grid min-w-0 flex-1 gap-1 text-xs">
			<span class="font-semibold">Named {kind} script</span>
			<select class="select select-bordered select-sm bg-base-100" bind:value={editorScriptId}>
				{#each scripts as script (script.id)}
					<option value={script.id}>{script.name}</option>
				{/each}
			</select>
		</label>
		<button class="btn btn-outline btn-sm" onclick={() => addScript(false)}>
			<Plus class="h-4 w-4" /> New
		</button>
		<button
			class="btn btn-outline btn-sm"
			disabled={!selectedScript}
			onclick={() => addScript(true)}
		>
			<Copy class="h-4 w-4" /> Duplicate
		</button>
	</div>

	{#if selectedScript}
		<div class="management-script-workbench">
			<div class="flex flex-wrap items-center justify-between gap-2">
				<div class="flex items-center gap-2">
					<FileCode2 class="h-4 w-4 text-library-manage" />
					<span class="text-xs text-base-content/55">
						{kind === 'naming' ? 'Changes paths, never tags.' : 'Changes metadata, never paths.'}
					</span>
				</div>
				<div class="flex items-center gap-1">
					<button class="btn btn-ghost btn-xs" onclick={toggleAttached} aria-pressed={attached}>
						{kind === 'naming'
							? attached
								? 'Selected'
								: 'Use this script'
							: attached
								? 'Attached'
								: 'Attach'}
					</button>
					{#if kind === 'tagging' && attached}
						<button
							class="btn btn-ghost btn-xs btn-square"
							aria-label="Run script earlier"
							disabled={selectedIds.indexOf(editorScriptId) === 0}
							onclick={() => moveAttached(-1)}><ArrowUp class="h-3.5 w-3.5" /></button
						>
						<button
							class="btn btn-ghost btn-xs btn-square"
							aria-label="Run script later"
							disabled={selectedIds.indexOf(editorScriptId) === selectedIds.length - 1}
							onclick={() => moveAttached(1)}><ArrowDown class="h-3.5 w-3.5" /></button
						>
					{/if}
				</div>
			</div>

			<label class="mt-3 grid gap-1 text-xs">
				<span class="font-semibold">Script name</span>
				<input
					class="input input-bordered input-sm bg-base-100"
					value={selectedScript.name}
					oninput={(event) => updateScript({ name: event.currentTarget.value })}
				/>
			</label>
			<label class="mt-3 grid gap-1 text-xs">
				<span class="font-semibold">Source</span>
				<textarea
					class="textarea textarea-bordered min-h-32 w-full bg-base-100 font-mono text-xs leading-relaxed"
					value={selectedScript.source}
					oninput={(event) => updateScript({ source: event.currentTarget.value })}
					aria-label={`${kind} script source`}
				></textarea>
			</label>
			{#if localProblem}
				<p class="mt-2 text-xs text-error" role="alert">{localProblem}</p>
			{:else}
				<p class="mt-2 text-xs text-success">
					Basic structure is valid. Full language validation runs before save.
				</p>
			{/if}

			<details class="mt-3 text-xs text-base-content/60">
				<summary class="cursor-pointer font-semibold">Language reference</summary>
				{#if kind === 'naming'}
					<p class="mt-2">
						Common values: title, album, artist, albumartist, year, disc, track, genre, ext, codec.
					</p>
					<p class="mt-1">
						Functions: default, conditional, pad, replace, lower, upper, title, join, ascii_fold,
						path_safe.
					</p>
					<p class="mt-1">
						Only literal / characters create directories. Actual paths and collisions appear in a
						dry run.
					</p>
					<div class="mt-3 grid gap-2 sm:grid-cols-2">
						<div class="rounded-lg border border-base-content/10 bg-base-100 p-2">
							<strong>Single artist</strong>
							<code class="mt-1 block break-all">{'{albumartist}/{title}.{ext}'}</code>
							<p class="mt-1">Alpha/Management Track.flac</p>
						</div>
						<div class="rounded-lg border border-base-content/10 bg-base-100 p-2">
							<strong>Multi-disc album</strong>
							<code class="mt-1 block break-all"
								>{'{albumartist}/{album} ({year})/{pad(disc, 2)}-{pad(track, 2)} {title}.{lower(extension)}'}</code
							>
							<p class="mt-1">Alpha/Management Album (2024)/01-02 Management Track.flac</p>
						</div>
					</div>
				{:else}
					<p class="mt-2">
						Statements: set, append, delete, if, else, end. Custom fields use custom.NAME.
					</p>
					<p class="mt-1">
						Scripts run in the attached order. Exact field-level effects are attributed in the
						management preview.
					</p>
					<div class="mt-3 grid gap-2 sm:grid-cols-2">
						<div class="rounded-lg border border-base-content/10 bg-base-100 p-2">
							<strong>Before</strong>
							<p class="mt-1">Title: the great escape</p>
							<p>Genre: Rock</p>
						</div>
						<div class="rounded-lg border border-base-content/10 bg-base-100 p-2">
							<strong>Script and result</strong>
							<code class="mt-1 block">set title = title(title)</code>
							<code class="block">append genre = "Alternative Rock"</code>
							<p class="mt-1">Title: The Great Escape</p>
							<p>Genre: Rock, Alternative Rock</p>
						</div>
					</div>
				{/if}
				<p class="mt-3 rounded-lg border border-warning/20 bg-warning/5 p-2">
					Run a dry preview for real file results, path-length and normalization warnings, and
					collision checks. The browser never guesses what a script will do.
				</p>
			</details>
			{#if selectedScript.preset_origin === 'legacy_naming_template'}
				<p class="mt-3 rounded-lg border border-info/20 bg-info/5 p-2 text-xs text-base-content/60">
					Imported from the legacy naming template and kept unchanged for compatibility.
				</p>
			{/if}
		</div>
	{:else}
		<div
			class="rounded-xl border border-dashed border-base-content/15 p-4 text-sm text-base-content/55"
		>
			No {kind} scripts exist yet. Create one to begin.
		</div>
	{/if}
</div>
