<script lang="ts">
	import { PackageOpen, LoaderCircle } from 'lucide-svelte';
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
	// the server's cap on one upload, all files together
	const LIMIT_BYTES = 8 * 1024 ** 3;

	const upload = uploadDropMutation();
	let dragOver = $state(false);
	let inputEl = $state<HTMLInputElement | null>(null);
	let progress = $state({ sent: 0, total: 0 });
	const sentAll = $derived(progress.total > 0 && progress.sent >= progress.total);

	function accepted(file: File): boolean {
		const dot = file.name.lastIndexOf('.');
		return dot >= 0 && ACCEPTED.includes(file.name.slice(dot).toLowerCase());
	}

	function megabytes(bytes: number): string {
		const mb = bytes / 1024 ** 2;
		return mb.toLocaleString(undefined, { maximumFractionDigits: mb < 10 ? 1 : 0 });
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
		const total = files.reduce((sum, file) => sum + file.size, 0);
		if (total > LIMIT_BYTES) {
			toastStore.show({ message: 'Uploads are limited to 8 GB.', type: 'error' });
			return;
		}
		progress = { sent: 0, total };
		upload.mutate({ files, onProgress: (sent, size) => (progress = { sent, total: size }) });
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

	const box = $derived(
		`flex w-full flex-col items-center justify-center gap-3 rounded-2xl border-2 border-dashed text-center ${compact ? 'p-5' : 'p-8 sm:p-10'}`
	);
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
	{#if upload.isPending}
		<div class="{box} h-full border-primary/50 bg-base-200/40" role="status">
			<LoaderCircle class="h-8 w-8 animate-spin motion-keep text-primary" aria-hidden="true" />
			<p class="text-sm font-medium">{sentAll ? 'Finishing upload…' : 'Uploading…'}</p>
			<progress
				class="progress progress-primary w-full max-w-xs"
				value={progress.sent}
				max={progress.total || 1}
				aria-label="Upload progress"
			></progress>
			<p class="text-xs tabular-nums text-base-content/60">
				{megabytes(progress.sent)} of {megabytes(progress.total)} MB
			</p>
		</div>
	{:else}
		<button
			type="button"
			class="group {box} transition-colors {dragOver
				? 'border-primary bg-primary/10'
				: 'border-base-content/20 bg-base-200/40 hover:border-primary/50 hover:bg-base-200'}"
			onclick={() => inputEl?.click()}
			aria-label="Import your purchased music"
		>
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
		</button>
	{/if}
	<input
		bind:this={inputEl}
		type="file"
		multiple
		accept={ACCEPTED.join(',')}
		aria-label="Choose files to import"
		class="hidden"
		{onchange}
	/>
</div>
