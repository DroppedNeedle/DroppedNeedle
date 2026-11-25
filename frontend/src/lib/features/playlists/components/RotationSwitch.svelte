<!--
	The person's switch for lists on rotation, on Profile. Off deletes the
	lists; on makes them again within a minute or so.
-->
<script lang="ts">
	import { authStore } from '$lib/stores/authStore.svelte';
	import { toastStore } from '$lib/stores/toast';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import {
		createSetRotationMutation,
		getRotationQuery
	} from '$lib/features/playlists/queries/rotation.svelte';

	const rotationQuery = getRotationQuery(() => authStore.isAuthenticated);
	const setRotation = createSetRotationMutation();

	async function save(enabled: boolean) {
		try {
			await setRotation.mutateAsync(enabled);
		} catch {
			toastStore.show({ message: "Couldn't change lists on rotation", type: 'error' });
		}
	}
</script>

{#if rotationQuery.data}
	<ToggleRow
		label="Lists on rotation"
		description="Five playlists from your library, changed every Monday."
		disabled={setRotation.isPending}
		bind:checked={() => rotationQuery.data?.enabled ?? true, (on) => void save(on)}
	/>
{/if}
