<script lang="ts" module>
	export type FormatMode = 'with' | 'without';
</script>

<script lang="ts">
	// The library lists' filter button: a minimum track count, and on the
	// album list the formats to keep (With) or hide (Without), each with its
	// album count. The menu changes the filters as it goes; the list follows.
	import { Check, Layers, ListFilter, X } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import Popover from '$lib/ui/Popover.svelte';
	import type { components } from '$lib/api/v3/openapi';

	type FormatCount = components['schemas']['AlbumFormatCount'];

	interface Props {
		/** Minimum tracks in the library; 1 keeps everything. */
		minTracks: number;
		onMinTracks: (min: number) => void;
		/** The album list's formats; absent on lists without a format filter. */
		formatCounts?: FormatCount[];
		formats?: string[];
		formatMode?: FormatMode;
		onFormats?: (formats: string[], mode: FormatMode) => void;
	}

	let {
		minTracks,
		onMinTracks,
		formatCounts = undefined,
		formats = [],
		formatMode = 'with',
		onFormats = undefined
	}: Props = $props();

	const PRESETS = [1, 2, 3, 5, 10];
	const MAX = 20;
	const sliderId = $props.id();

	const active = $derived((minTracks > 1 ? 1 : 0) + (formats.length > 0 ? 1 : 0));
	const label = (min: number) => (min <= 1 ? 'Any' : `${min}+`);
	// On the album list the button reads as the format select it leads with.
	const buttonLabel = $derived.by(() => {
		if (!formatCounts || !onFormats) return 'Filter';
		if (formats.length === 0) return 'All formats';
		const list = formats.map((format) => format.toUpperCase()).join(', ');
		return formatMode === 'without' ? `Without ${list}` : list;
	});

	function toggleFormat(format: string) {
		const next = formats.includes(format)
			? formats.filter((known) => known !== format)
			: [...formats, format];
		onFormats?.(next, formatMode);
	}

	function reset() {
		onMinTracks(1);
		onFormats?.([], 'with');
	}
</script>

<Popover
	label={buttonLabel}
	title="Filters"
	icon={formatCounts && onFormats ? Layers : ListFilter}
	caret={Boolean(formatCounts && onFormats)}
	count={active}
	class={formatCounts && onFormats ? 'sm:w-58' : ''}
>
	{#snippet children(close)}
		<div class="space-y-2">
			<label class="block text-sm font-semibold" for={sliderId}>Minimum tracks you have</label>
			<input
				id={sliderId}
				type="range"
				min="1"
				max={MAX}
				class="range range-sm range-primary"
				value={Math.min(minTracks, MAX)}
				aria-valuetext={label(minTracks)}
				onchange={(event) => onMinTracks(Number((event.target as HTMLInputElement).value))}
			/>
			<div class="flex flex-wrap gap-1">
				{#each PRESETS as preset (preset)}
					<Button
						variant={minTracks === preset ? 'primary' : 'ghost'}
						size="xs"
						active={minTracks === preset}
						aria-pressed={minTracks === preset}
						onclick={() => onMinTracks(preset)}>{label(preset)}</Button
					>
				{/each}
				{#if !PRESETS.includes(minTracks)}
					<span class="badge badge-primary badge-sm self-center">{label(minTracks)}</span>
				{/if}
			</div>
		</div>

		{#if formatCounts && onFormats}
			<div class="divider my-0"></div>
			<div class="space-y-2">
				<div class="flex items-center justify-between">
					<p class="text-sm font-semibold">Formats</p>
					<div class="join" role="radiogroup" aria-label="Format filter mode">
						{#each [['with', 'With'], ['without', 'Without']] as [mode, text] (mode)}
							<Button
								variant={formatMode === mode ? 'primary' : 'ghost'}
								size="xs"
								active={formatMode === mode}
								class="join-item"
								role="radio"
								aria-checked={formatMode === mode}
								onclick={() => onFormats(formats, mode as FormatMode)}>{text}</Button
							>
						{/each}
					</div>
				</div>
				<div class="flex flex-wrap gap-2">
					{#each formatCounts as { format, album_count } (format)}
						{@const marked = formats.includes(format)}
						<Button
							variant={marked ? (formatMode === 'without' ? 'danger' : 'primary') : 'outline'}
							size="sm"
							icon={marked ? (formatMode === 'without' ? X : Check) : undefined}
							active={marked}
							aria-pressed={marked}
							onclick={() => toggleFormat(format)}
							>{format.toUpperCase()}
							<span class="font-normal opacity-60">{album_count}</span></Button
						>
					{/each}
				</div>
				<p class="text-xs text-muted">
					{formatMode === 'without'
						? 'Hides the marked formats.'
						: 'Shows only the marked formats.'}
				</p>
			</div>
		{/if}

		<div class="flex justify-between pt-1">
			<Button variant="ghost" size="xs" onclick={reset} disabled={!active}>Reset</Button>
			<Button variant="primary" size="xs" onclick={close}>Done</Button>
		</div>
	{/snippet}
</Popover>
