<script lang="ts">
	import { Plus } from 'lucide-svelte';
	import type { Snippet } from 'svelte';

	import { withBasePath } from '$lib/utils/basePath';

	interface Props {
		icon: Snippet;
		label: string;
		settingsTab: string;
	}

	let { icon, label, settingsTab }: Props = $props();
</script>

<li class="opacity-60 transition-opacity duration-150 hover:opacity-100">
	<a
		href={withBasePath(`/settings?tab=${settingsTab}`)}
		class="is-drawer-close:tooltip is-drawer-close:tooltip-right"
		data-tip="Connect {label}"
		aria-label="Connect {label}"
	>
		<div class="relative">
			{@render icon()}
			<span
				class="absolute -right-1 -bottom-1 flex items-center justify-center rounded-full bg-base-300 p-0.5"
			>
				<Plus class="w-2.5 h-2.5" />
			</span>
		</div>
		<span class="is-drawer-close:hidden">{label}</span>
	</a>
</li>
