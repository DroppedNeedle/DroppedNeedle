<!--
	The year's share image: pick Story or Square, see it, save it, or share
	it where the browser can share a file. The image is drawn on this device
	from the review the page already holds. Each PNG is made as soon as the
	sheet opens or the shape changes, so Share runs straight inside the
	press with its file ready (Safari drops the press across a wait).
-->
<script lang="ts">
	import { untrack } from 'svelte';
	import { Download, Share2 } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import SegmentedChoice from '$lib/ui/SegmentedChoice.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { saveBlob } from '$lib/utils/blobDownload';
	import type { YearReview } from '../queries/year.svelte';
	import type { ImageShape, LoadImage } from '../state/shareImage';

	interface Props {
		review: YearReview;
		open: boolean;
	}

	let { review, open = $bindable(false) }: Props = $props();

	interface Made {
		shape: ImageShape;
		file: File;
		url: string;
		canShare: boolean;
	}

	const shapes: { value: ImageShape; label: string }[] = [
		{ value: 'story', label: 'Story' },
		{ value: 'square', label: 'Square' }
	];

	let shape = $state<ImageShape>('story');
	let made = $state<Made | null>(null);
	let failed = $state(false);
	// The covers read while the sheet is open, kept for the other shape.
	let covers: { load: LoadImage; close: () => void } | null = null;

	function canShare(file: File): boolean {
		try {
			return typeof navigator.canShare === 'function' && navigator.canShare({ files: [file] });
		} catch {
			return false;
		}
	}

	function dropImage() {
		if (made) URL.revokeObjectURL(made.url);
		made = null;
	}

	function release() {
		dropImage();
		covers?.close();
		covers = null;
	}

	$effect(() => {
		if (!open) {
			untrack(release);
			return;
		}
		const chosen = shape;
		const year = review;
		let current = true;
		untrack(dropImage);
		failed = false;
		void (async () => {
			try {
				// Each step stops once the sheet has closed or the shape changed.
				const art = await import('../state/shareImage');
				if (!current) return;
				covers ??= art.onceEach(art.loadImageFromApi);
				const canvas = await art.drawYear(year, chosen, covers.load);
				if (!current) return;
				const png = await art.toPng(canvas);
				if (!current) return;
				const file = new File([png], `droppedneedle-${year.year}-${chosen}.png`, {
					type: 'image/png'
				});
				made = { shape: chosen, file, url: URL.createObjectURL(png), canShare: canShare(file) };
			} catch {
				if (current) failed = true;
			}
		})();
		return () => {
			current = false;
		};
	});

	$effect(() => () => untrack(release));

	function save() {
		if (made) saveBlob(made.file, made.file.name);
	}

	function share() {
		if (!made) return;
		navigator.share({ files: [made.file] }).catch((error: unknown) => {
			if (error instanceof DOMException && error.name === 'AbortError') return;
			toastStore.notify('The image could not be shared.', 'error');
		});
	}
</script>

<Modal bind:open title="Save image" paper bottomSheet closeButton>
	<div class="flex flex-col gap-4">
		<SegmentedChoice label="Shape" options={shapes} bind:value={shape} />
		<div class="flex justify-center">
			{#if made}
				<img
					src={made.url}
					alt="Preview of the {made.shape} image"
					class={[
						'w-54 bg-paper-content',
						made.shape === 'story' ? 'aspect-[9/16]' : 'aspect-square'
					]}
				/>
			{:else if failed}
				<p role="alert" class="py-8 text-sm">The image could not be drawn.</p>
			{:else}
				<div
					class={[
						'skeleton w-54 rounded-none',
						shape === 'story' ? 'aspect-[9/16]' : 'aspect-square'
					]}
					role="status"
					aria-label="Drawing the image"
				></div>
			{/if}
		</div>
		<div class="flex gap-3">
			<Button
				variant="primary"
				size="display"
				icon={Download}
				class="flex-1"
				disabled={!made}
				onclick={save}
			>
				Save
			</Button>
			{#if made?.canShare}
				<Button variant="paper" size="display" icon={Share2} class="flex-1" onclick={share}>
					Share
				</Button>
			{/if}
		</div>
		<p class="text-center font-mono text-xs tracking-wider uppercase">
			Made on this device. Nothing is uploaded.
		</p>
	</div>
</Modal>
