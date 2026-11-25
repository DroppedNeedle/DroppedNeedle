<!-- A pick card and the listening post wired to the sampler as the Discover page wires them. -->
<script lang="ts">
	import type { SetPick } from '$lib/features/discover/queries/week.svelte';
	import { deckSampler } from '$lib/features/discover/state/deckSampler.svelte';
	import ListeningPost from './ListeningPost.svelte';
	import PickCard from './PickCard.svelte';

	interface Props {
		pick: SetPick;
	}

	let { pick }: Props = $props();

	const key = $derived(pick.album.musicbrainz_id ?? '');
	const sampling = $derived(deckSampler.activeKey === key && deckSampler.status !== 'idle');
	const noop = () => {};

	function onsample() {
		if (sampling) {
			deckSampler.stop();
			return;
		}
		deckSampler.start(key, pick.album.artist_name ?? '', pick.album.title, {
			albumMbid: key,
			mode: 'audition'
		});
	}
</script>

<PickCard
	album={pick.album}
	reason={pick.reason}
	tapes={['New to you']}
	kicker="No. 03"
	pager={{ text: '3 / 8', spoken: 'Pick 3 of 8' }}
	recordLabel="WK 47 / NO 3"
	{sampling}
	hasPrevious={false}
	hasNext={false}
	keeping={false}
	answersOff={false}
	onprevious={noop}
	onnext={noop}
	{onsample}
	onopened={noop}
	onwhy={noop}
	onkeep={noop}
	onlater={noop}
	onno={noop}
/>
<ListeningPost onweek={noop} />
