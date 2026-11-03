<script lang="ts" module>
	/** One format the album list has, with its album count. */
	export interface FormatCount {
		format: string;
		album_count: number;
	}

	export type FormatMode = 'with' | 'without';
</script>

<script lang="ts">
	// The library lists' filter button: a minimum track count, and on the
	// album list the formats to keep (With) or hide (Without), each with its
	// album count. The menu changes the filters as it goes; the list follows.
	import { Check, ListFilter, X } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';

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
	let open = $state(false);
	let root: HTMLDivElement | undefined = $state();

	const active = $derived((minTracks > 1 ? 1 : 0) + (formats.length > 0 ? 1 : 0));
	const label = (min: number) => (min <= 1 ? 'Any' : `${min}+`);

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

	// Close on a click outside the menu or on Escape.
	$effect(() => {
		if (!open) return;
		const away = (event: PointerEvent) => {
			if (root && !root.contains(event.target as Node)) open = false;
		};
		const escape = (event: KeyboardEvent) => {
			if (event.key === 'Escape') open = false;
		};
		document.addEventListener('pointerdown', away);
		document.addEventListener('keydown', escape);
		return () => {
			document.removeEventListener('pointerdown', away);
			document.removeEventListener('keydown', escape);
		};
	});
</script>

<div class="relative" bind:this={root}>
	<Button
		variant={active ? 'primary' : 'outline'}
		icon={ListFilter}
		aria-expanded={open}
		aria-haspopup="dialog"
		onclick={() => (open = !open)}
		>Filter{#if active}<span class="badge badge-sm ml-1">{active}</span>{/if}</Button
	>
	{#if open}
		<div
			class="absolute right-0 z-30 mt-2 w-80 max-w-[calc(100vw-2rem)] space-y-4 rounded-box bg-base-100 p-4 shadow-xl"
			role="dialog"
			aria-label="Filters"
		>
			<div class="space-y-2">
				<p class="text-sm font-semibold" id="min-tracks-label">Minimum tracks in library</p>
				<input
					type="range"
					min="1"
					max={MAX}
					class="range range-sm range-primary"
					value={Math.min(minTracks, MAX)}
					aria-labelledby="min-tracks-label"
					aria-valuetext={label(minTracks)}
					oninput={(event) => onMinTracks(Number((event.target as HTMLInputElement).value))}
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
					<p class="text-xs text-base-content/50">
						{formatMode === 'without'
							? 'Hides albums in the marked formats.'
							: 'Shows only albums in the marked formats.'} None marked: every format.
					</p>
				</div>
			{/if}

			<div class="flex justify-between pt-1">
				<Button variant="ghost" size="xs" onclick={reset} disabled={!active}>Reset</Button>
				<Button variant="primary" size="xs" onclick={() => (open = false)}>Done</Button>
			</div>
		</div>
	{/if}
</div>
