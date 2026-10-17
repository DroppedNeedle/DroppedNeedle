<script lang="ts">
	import { ArrowDown, ArrowUp, Copy, FileCode, Plus } from 'lucide-svelte';

	import type { ManagementScriptSettings } from '$lib/queries/library-management/types';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import TextField from '$lib/ui/TextField.svelte';
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
	const namingAssignment = $derived.by(() => {
		if (kind !== 'naming') return '';
		const single = selectedIds[0] === editorScriptId;
		const multi = selectedIds[1] === editorScriptId;
		if (single && multi) return 'Assigned to single-disc and multi-disc';
		if (single) return 'Assigned to single-disc';
		if (multi) return 'Assigned to multi-disc';
		return 'Not assigned';
	});
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
					? '{albumartist}/{album}{conditional(is_empty(year), "", concat(" (", year, ")"))}/{track:02d} - {title}.{ext}'
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
		onchange([...scripts, script], selectedIds);
	}

	function toggleAttached(): void {
		if (!selectedScript) return;
		if (kind === 'naming') return;
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
		<div class="min-w-0 flex-1">
			<SelectField
				label="Script currently being edited"
				options={scripts.map((script) => ({ value: script.id, label: script.name }))}
				bind:value={editorScriptId}
			/>
		</div>
		<Button variant="outline" size="sm" icon={Plus} onclick={() => addScript(false)}>New</Button>
		<Button
			variant="outline"
			size="sm"
			icon={Copy}
			disabled={!selectedScript}
			onclick={() => addScript(true)}>Duplicate</Button
		>
	</div>

	{#if selectedScript}
		<div class="management-script-workbench">
			<div class="flex flex-wrap items-center justify-between gap-2">
				<div class="flex items-center gap-2">
					<FileCode class="h-4 w-4 text-library-manage" />
					<span class="text-xs text-base-content/55">
						{kind === 'naming' ? 'Changes paths, never tags.' : 'Changes metadata, never paths.'}
					</span>
				</div>
				<div class="flex items-center gap-1">
					{#if kind === 'naming'}<span class="badge badge-ghost badge-sm">{namingAssignment}</span
						>{/if}
					{#if kind === 'tagging'}
						<Button variant="ghost" size="xs" onclick={toggleAttached} aria-pressed={attached}>
							{attached ? 'Attached' : 'Attach'}
						</Button>
					{/if}
					{#if kind === 'tagging' && attached}
						<IconButton
							size="xs"
							shape="square"
							icon={ArrowUp}
							label="Run script earlier"
							disabled={selectedIds.indexOf(editorScriptId) === 0}
							onclick={() => moveAttached(-1)}
						/>
						<IconButton
							size="xs"
							shape="square"
							icon={ArrowDown}
							label="Run script later"
							disabled={selectedIds.indexOf(editorScriptId) === selectedIds.length - 1}
							onclick={() => moveAttached(1)}
						/>
					{/if}
				</div>
			</div>

			<div class="mt-3">
				<TextField
					label="Script name"
					bind:value={() => selectedScript.name, (name) => updateScript({ name })}
				/>
			</div>
			<label class="mt-3 grid gap-1 text-xs">
				<span class="font-semibold">Source</span>
				<textarea
					class="textarea min-h-32 w-full font-mono text-xs leading-relaxed"
					value={selectedScript.source}
					oninput={(event) => updateScript({ source: event.currentTarget.value })}
					aria-label={`${kind} script source`}></textarea>
			</label>
			{#if localProblem}
				<p class="mt-2 text-xs text-error" role="alert">{localProblem}</p>
			{:else}
				<p class="mt-2 text-xs text-success">
					Basic structure is valid. Full language validation runs before save.
				</p>
			{/if}
			{#if kind === 'naming'}
				<div class="mt-3 rounded-lg border border-base-content/10 bg-base-100 p-3 text-xs">
					<strong>Built-in Picard-style examples</strong>
					<p class="mt-1 font-mono">Artist/Album (2008)/03 - Title.flac</p>
					<p class="font-mono">Artist/Album (2023)/CD 01/01 - Track.flac</p>
					<p class="font-mono">Artist/Album (2023)/Disc 01/01 - Track.flac</p>
				</div>
			{/if}

			<details class="mt-3 text-xs text-base-content/60">
				<summary class="cursor-pointer font-semibold">Language reference</summary>
				{#if kind === 'naming'}
					<p class="mt-2">
						Common values: title, album, album_disambiguation, artist, albumartist, initial, year,
						disc, track, medium_format, medium_number, genre, ext, codec.
					</p>
					<p class="mt-1">
						{'{initial}'} uses the effective album artist, ignores leading Unicode whitespace and a case-insensitive
						"The" plus following Unicode whitespace, then returns one uppercase letter; empty or nonletter
						leads return "#".
					</p>
					<p class="mt-1">
						Functions: default, conditional, concat, is_empty, pad, replace, lower, upper, title,
						join, ascii_fold, path_safe.
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
							<strong>Built-in multi-disc shape</strong>
							<code class="mt-1 block break-all"
								>{'{albumartist}/{album}{conditional(is_empty(year), "", concat(" (", year, ")"))}{conditional(is_empty(album_disambiguation), "", concat(" (", album_disambiguation, ")"))}/{default(medium_format, "Disc")} {medium_number:02d}/{track:02d} - {title}.{ext}'}</code
							>
							<p class="mt-1">Alpha/Management Album (2023)/CD 01/02 - Management Track.flac</p>
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

<style>
	.management-script-editor {
		display: grid;
		gap: 0.75rem;
	}

	.management-script-toolbar {
		display: flex;
		align-items: flex-end;
		gap: 0.5rem;
	}

	.management-script-workbench {
		padding: 0.85rem;
		border: 1px solid color-mix(in oklab, var(--color-library-manage) 18%, transparent);
		border-radius: 0.8rem;
		background: color-mix(in oklab, var(--color-base-200) 58%, var(--color-base-100));
	}

	@media (max-width: 639px) {
		.management-script-toolbar {
			align-items: stretch;
			flex-direction: column;
		}
	}
</style>
