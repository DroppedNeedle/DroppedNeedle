<script lang="ts">
	import { PackageOpen, Loader2 } from 'lucide-svelte';
	import { uploadDropMutation } from '$lib/queries/import/DropImportMutations.svelte';
	import { toastStore } from '$lib/stores/toast';

	interface Props {
		compact?: boolean;
		className?: string;
	}
	let { compact = false, className = '' }: Props = $props();

	// mirror the backend's accepted extensions (route-level validation)
	const ACCEPTED = [
		'.zip',
		'.flac',
		'.mp3',
		'.m4a',
		'.m4b',
		'.mp4',
		'.ogg',
		'.oga',
		'.opus',
		'.wav'
	];

	const upload = uploadDropMutation();
	let dragOver = $state(false);
	let inputEl = $state<HTMLInputElement | null>(null);

	function accepted(file: File): boolean {
		const dot = file.name.lastIndexOf('.');
		return dot >= 0 && ACCEPTED.includes(file.name.slice(dot).toLowerCase());
	}

	function submit(list: FileList | null) {
		if (!list || upload.isPending) return;
		const files = Array.from(list).filter(accepted);
		if (!files.length) {
			toastStore.show({
				message: 'Drop zips or audio files - folders need zipping first.',
				type: 'info'
			});
			return;
		}
		upload.mutate(files);
	}

	function ondrop(event: DragEvent) {
		event.preventDefault();
		dragOver = false;
		submit(event.dataTransfer?.files ?? null);
	}

	function onchange(event: Event) {
		const input = event.currentTarget as HTMLInputElement;
		submit(input.files);
		input.value = '';
	}
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
	class="relative {className}"
	ondragover={(e) => {
		e.preventDefault();
		dragOver = true;
	}}
	ondragleave={() => (dragOver = false)}
	{ondrop}
>
	<button
		type="button"
		class="group flex w-full flex-col items-center justify-center gap-3 rounded-2xl border-2 border-dashed text-center transition-colors {compact
			? 'p-5'
			: 'p-8 sm:p-10'} {dragOver
			? 'border-primary bg-primary/10'
			: 'border-base-content/20 bg-base-200/40 hover:border-primary/50 hover:bg-base-200'}"
		onclick={() => inputEl?.click()}
		disabled={upload.isPending}
		aria-label="Import your purchased music"
	>
		{#if upload.isPending}
			<Loader2 class="h-8 w-8 animate-spin text-primary" aria-hidden="true" />
			<p class="text-sm font-medium">Uploading…</p>
		{:else}
			<div
				class="flex h-12 w-12 items-center justify-center rounded-2xl bg-primary/15 ring-1 ring-primary/30 transition-transform duration-300 group-hover:-rotate-6 group-hover:scale-110"
			>
				<PackageOpen class="h-6 w-6 text-primary" aria-hidden="true" />
			</div>
			<div>
				<p class="font-semibold {compact ? 'text-base' : 'text-lg'}">Drop your purchases here</p>
				<p class="mt-0.5 text-xs text-base-content/60">
					Zips or audio files from Bandcamp, Qobuz, or anywhere else. Click to browse.
				</p>
			</div>
		{/if}
	</button>
	<input
		bind:this={inputEl}
		type="file"
		multiple
		accept={ACCEPTED.join(',')}
		class="hidden"
		{onchange}
	/>
</div>
