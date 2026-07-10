<script lang="ts">
	import { Plus } from 'lucide-svelte';
	import type { Snippet } from 'svelte';

	interface Props {
		icon: Snippet;
		label: string;
		settingsTab: string;
	}

	let { icon, label, settingsTab }: Props = $props();
</script>

<li>
	<a
		href="/settings?tab={settingsTab}"
		class="dn-navitem dn-navitem--hint"
		data-tip="Connect {label}"
	>
		<span class="dn-navitem__iconwrap">
			{@render icon()}
			<span class="dn-navitem__addbadge">
				<Plus class="h-2.5 w-2.5" />
			</span>
		</span>
		<span class="dn-navitem__label">{label}</span>
	</a>
</li>
