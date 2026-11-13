// deno run --allow-read plugin/tests/connection-toast.test.js
const source = Deno.readTextFileSync(new URL('../ConnectionToast.js', import.meta.url))
  .replace(/^\.(?:pragma|import)[^\n]*\n/gm, '')
const modelSource = Deno.readTextFileSync(new URL('../Model.js', import.meta.url))
const Model = new Function(modelSource + '; return {parseStatus,routeRequesting,acquiringPlayback}')()
const T = new Function('Model', source + '; return {initial,step,project,presentation,text,label,acquire,release,advance,preview,LIMIT_MS}')(Model)
function eq(actual, expected, message) {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) throw Error(message + ': ' + JSON.stringify(actual))
}
const idle = {connected: true, ownershipIntent: 'release', connectionLifecycle: {state: 'control_ready'},
  modelName: 'AirPods Max 2', isHeadset: true, headset: {level: 93}}
const linking = {...idle, ownershipIntent: 'claim', connectionLifecycle: {state:'linking'}}
const playing = {...linking, connectionLifecycle: {state:'streaming'}, audioTransportActive:true}
const p = (s, now=1000) => T.project(s, true, now)
const attached = {attached:true}
let s = T.step(T.initial(), p(idle), 1000)
eq(s.visible, false, 'Background connections stay quiet')
s = T.step(s, p(linking), 2000)
eq(s.visible, true, 'Playback connection opens toast')
s = T.step(s, p({...linking, connectionLifecycle:{state:'retrying'}}), 3000)
eq(s.started, 2000, 'Retry uses the same toast')
s = T.step(s, p(playing), 4000)
eq(s.phase, 'playing', 'Confirmed transport completes toast')
eq(s.visible, true, 'Completion is briefly visible')
s = T.step(s, p(playing), 5000)
eq(s.visible, false, 'Completion dismisses')
eq(T.step(T.initial(), p(playing), 1000).visible, false, 'Startup during playback is quiet')
s = T.step(T.initial(), p(linking), 1000)
s = T.step(s, p(linking), 2000, {dismissed:true})
eq(s.visible, false, 'Manual dismissal closes')
s = T.step(s, p(linking), 3000)
eq(s.visible, false, 'Status updates do not reopen dismissed attempt')
s = T.step(s, p(playing), 4000)
s = T.step(s, p(linking), 5000)
eq(s.visible, true, 'Next attempt can notify')
s = T.step(s, p(linking), 35000)
eq(s.visible, false, 'Stalled attempt expires')
eq(T.step(s, p(linking), 36000).visible, false, 'Stall does not restart toast')
eq(p({...playing, audioTransportActive:false}).phase === 'playing', false, 'Connected does not imply audio')
eq(p({...idle, connected:false, lastAdvertisementMs:1}, 400000).battery, -1, 'Stale batteries omitted')
eq(p({...idle, headset:{level:-1}}).battery, -1, 'Unknown batteries omitted')
eq(p({...idle, isHeadset:false, left:{level:70}, right:{level:60}}).battery, 60, 'Lower bud charge shown')
const evil = '<img src="https://example.invalid"> &lt;b&gt; --exec\n'
const text = T.text(p({...linking, modelName:evil}))
eq(/[<>&\n]/.test(text.summary), false, 'Dynamic names cannot become native markup or entities')
eq(T.acquire('monitor-a'), true, 'First panel owns publisher')
eq(T.acquire('monitor-b'), false, 'Second panel cannot publish')
T.release('monitor-b')
eq(T.acquire('monitor-b'), false, 'Nonowner cannot release publisher')
T.release('monitor-a')
eq(T.acquire('monitor-b'), true, 'Another monitor takes over after destruction')
console.log('Connection toast lifecycle, markup and publisher checks passed')
const waiting = {...linking, connectionLifecycle: {state:'routing', waiting_for:'output_running'}}
eq(T.step(T.initial(), p(waiting), 1000).visible, false, 'Paused established route does not announce a connection')
s = T.step(T.initial(), p(waiting), 1000)
eq(T.step(s, p(linking), 2000).visible, true, 'A reconnect after initial paused state can notify')
s = T.step(T.initial(), p(linking), 1000)
s = T.step(s, p(waiting), 2000)
eq(s.visible, true, 'An ongoing attempt can wait for audio')
s = T.step(s, p(waiting), 11000)
eq(s.visible, false, 'Waiting for playback is bounded')
s = T.step(T.initial(), p(linking), 1000)
s = T.step(s, p({...linking, deviceName:'other', modelName:'Other pods'}), 2000)
eq(s.started, 2000, 'A different device is a fresh attempt')
s = T.step(T.initial(), p(linking), 1000)
s = T.step(s, T.project(linking, false, 2000), 2000)
eq(s.phase, 'offline', 'Daemon loss interrupts toast')
s = T.step(s, T.project(linking, false, 5000), 5000)
eq(s.visible, false, 'Daemon loss dismisses after feedback')
eq(Model.parseStatus(JSON.stringify({schema_version:1,connection_lifecycle:{state:'streaming',waiting_for:''}})).connectionLifecycle,
  {state:'streaming',waiting_for:''}, 'Daemon lifecycle survives status parsing')
console.log('Waiting, device-switch, daemon-loss and status-parsing checks passed')

// A brief running-output interruption belongs to the same visible operation.
// Its initial publication may still be in flight when these observations arrive.
s = T.step(T.initial(), p(linking), 1000)
const episode = s.started
s = T.step(s, p(playing), 1300)
s = T.step(s, p({...linking, connectionLifecycle:{state:'routing',waiting_for:'stream_route'}}), 1500)
eq(s.started, episode, 'Route recovery retains the outstanding publication identity')
eq(s.ended, 0, 'Resumed progress cancels the old completion deadline')
s = T.step(s, p(waiting), 2000)
s = T.step(s, p(playing), 3000)
eq(s.visible, true, 'Playing updates the existing visible toast')
s = T.step(s, p(playing), 3999)
eq(s.visible, true, 'Completion gets its full display time after recovery')
s = T.step(s, p(playing), 4000)
eq(s.visible, false, 'One toast dismisses after settled completion')
console.log('In-place recovery and completion checks passed')

// A fast connection waits for the half-second rear hold, one-second turn and final hold, while genuine
// connection progress and manual dismissal retain their existing semantics.
s = T.step(T.initial(), p(linking), 1000)
s = T.step(s, p(playing), 1100, attached)
s = T.step(s, p(playing), 1500, attached)
eq(s.visible, true, 'Initial rear hold does not allow early dismissal')
s = T.step(s, p(playing), 2500, attached)
eq(s.visible, true, 'Finishing the turn does not skip the final pose hold')
s = T.step(s, p(playing), 3599, attached)
eq(s.visible, true, 'Final pose remains visible for a full second')
s = T.step(s, p(playing), 3600, attached)
eq(s.visible, false, 'Completed turn, hold and playback dismiss together')
s = T.step(T.initial(), p(linking), 1000)
s = T.step(s, p(linking), 3500, attached)
eq(s.visible, true, 'Finished artwork cannot hide a pending connection')
s = T.step(s, p(linking), 3600, {attached:true,dismissed:true})
eq(s.visible, false, 'Manual dismissal does not wait for the turn')
console.log('Artwork completion gate checks passed')

// File watches can coalesce all intermediate routing states during a takeover.
s = T.step(T.initial(), p(idle), 1000)
s = T.step(s, p(playing), 1100, attached)
eq(s.visible, true, 'Remote-to-local playback announces even with no observed busy state')
eq(s.phase, 'playing', 'Fast takeover reports the actual completed phase')
const fastStarted = s.started
s = T.step(s, p(playing), 2200, attached)
eq(s.visible, true, 'Fast takeover still gets the complete artwork animation')
s = T.step(s, p(playing), 3600, attached)
eq(s.visible, false, 'Fast takeover dismisses after the artwork hold')
s = T.step(s, p(playing), 4000)
eq(s.started, fastStarted, 'Repeated streaming observations do not republish')
eq(s.visible, false, 'Stable playback stays quiet')
s = T.step(s, p(waiting), 4100)
s = T.step(s, p(playing), 4200)
eq(s.visible, false, 'Pause and resume of the same playback claim stay quiet')
s = T.step(s, p(idle), 4300)
s = T.step(s, p(playing), 4400)
eq(s.visible, true, 'The next fleet takeover announces again')
s = T.step(s, p(playing), 4500, {dismissed:true})
s = T.step(s, p(playing), 4600)
eq(s.visible, false, 'Manual dismissal also suppresses a fast takeover')
s = T.step(s, p({...playing, deviceName:'second', modelName:'Other pods'}), 4700)
eq(s.visible, true, 'A different accessory already playing gets its own toast')
s = T.step(T.initial(), T.project(playing, false, 1000), 1000)
s = T.step(s, p(playing), 1100)
eq(s.visible, false, 'Initial offline observation does not announce established playback on load')
s = T.step(T.initial(), p(idle), 1000)
s = T.step(s, p({...idle, ownershipIntent:'claim'}), 1100)
s = T.step(s, p(playing), 1200)
eq(s.visible, true, 'An early claim before routing cannot consume the takeover trigger')
console.log('Fast takeover, resume, dismissal and startup checks passed')

// Gram began asking its peer four seconds before the local lifecycle linked.
const backgroundAdmission = {...idle, connected:false, ownershipIntent:'none', wantOmarchySeat:false,
  connectionLifecycle:{state:'availability',waiting_for:'link_admission'},
  fleet:{routing:{phase:'waiting_admission',busy:true}}}
// Desktop input joins in the background without requesting the audio seat.
s = T.step(T.initial(), p(idle), 1000)
for (const [phase, state] of [['waiting_admission','availability'], ['connecting','linking'],
  ['waiting_completion','preparing_paths'], ['success','control_ready']]) {
  s = T.step(s, p({...backgroundAdmission, connectionLifecycle:{state},
    fleet:{routing:{phase,busy:phase !== 'success'}}}), 2000)
  eq(s.visible, false, 'Background activity stays quiet through ' + phase)
}
s = T.step(s, p(playing), 3000)
eq(s.visible, true, 'Fast playback takeover after a quiet background join still notifies')
const admission = {...backgroundAdmission, ownershipIntent:'claim', wantOmarchySeat:true}
s = T.step(T.initial(), p(admission), 1000)
eq(s.visible, true, 'A playback request starts the toast before admission completes')
eq(s.phase, 'connecting', 'Admission wait is presented as connecting')
const admissionStarted = s.started
for (const phase of ['connecting','waiting_completion']) {
  s = T.step(s, p({...admission, fleet:{routing:{phase,busy:true}}}), 2000)
  eq(s.started, admissionStarted, 'Fleet phases stay in the same toast')
}
s = T.step(s, p(linking), 5000)
eq(s.started, admissionStarted, 'Bluetooth setup continues the admission toast')
s = T.step(s, p(playing), 6000)
eq(s.started, admissionStarted, 'Streaming completes the same admission toast')
for (const phase of ['admitting','waiting_connection','success','idle']) {
  const helper = {...admission, fleet:{routing:{phase,busy:true}}}
  eq(T.step(T.initial(), p(helper), 1000).visible, false,
    'Helping another laptop or a finished request stays quiet: ' + phase)
}
eq(T.step(T.initial(), p({...admission, fleet:{routing:{phase:'waiting_admission',busy:false}}}), 1000).visible,
  false, 'An inactive admission record cannot start a toast')
s = T.step(T.initial(), p(admission), 1000)
s = T.step(s, p({...admission, fleet:{routing:{phase:'failed',busy:false}}}), 2000)
eq(s.phase, 'failed', 'An admission failure ends with failure feedback')
s = T.step(s, p({...admission, fleet:{routing:{phase:'failed',busy:false}}}), 5000)
eq(s.visible, false, 'Admission failure feedback expires')
eq(p({...playing, fleet:{routing:{phase:'failed',busy:false}}}).phase, 'playing',
  'A retained failure from an older request cannot override current playback')
console.log('Early fleet admission and helper exclusion checks passed')

for (const intent of [{ownershipIntent:'claim'}, {wantOmarchySeat:true}]) {
  s = T.step(T.initial(), p(backgroundAdmission), 1000)
  const pending = {...backgroundAdmission, fleet:{routing:{phase:'waiting_completion',busy:true}}}
  s = T.step(s, p({...pending,...intent}), 2000)
  eq(s.visible, true, 'An audio claim during a background join starts the toast immediately')
  eq(s.started, 2000, 'Quiet background setup does not consume the playback trigger')
  s = T.step(s, p(playing), 3000)
  eq(s.started, 2000, 'Playback completes the toast started by the pending claim')
}
eq(Model.parseStatus(JSON.stringify({schema_version:1,want_omarchy_seat:true})).wantOmarchySeat,
  true, 'Queued audio intent survives status parsing')
console.log('Background join and queued playback claim checks passed')

// A slow handoff starts its artwork only on Playing and keeps that same card.
s = T.step(T.initial(), p(linking), 1000, attached)
s = T.step(s, p(linking), 4000, attached)
s = T.step(s, p(playing), 5000, attached)
eq(s.visible, true, 'Late Playing begins completion in the existing toast')
const slowEpisode = s.started
s = T.step(s, p(playing), 6000, attached)
eq(s.visible, true, 'Playing stays visible while its turn runs')
s = T.step(s, p(playing), 7499, attached)
eq(s.visible, true, 'The completion turn receives its full final hold')
s = T.step(s, p(playing), 7500, attached)
eq(s.visible, false, 'Completion turn and final hold finish before dismissal')
eq(s.started, slowEpisode, 'Completion does not create a second notification episode')
s = T.step(T.initial(), p(linking), 1000, attached)
s = T.step(s, p(idle), 2000, attached)
s = T.step(s, p(playing), 4500, attached)
eq(s.visible, true, 'Delayed Playing cannot inherit an earlier cancellation deadline')
eq(s.ended, 4500, 'Playing starts a fresh completion hold')
s = T.step(s, p(playing), 4600, {attached:true,dismissed:true})
eq(s.visible, false, 'Manual dismissal overrides the completion turn')
console.log('Playback-only completion hold checks passed')

// The same model now owns both the pose and dismissal; native attachment only
// supplies an observation and cannot start or reset a separate clock.
s = T.step(T.initial(), p(linking), 1000, attached)
s = T.step(s, p(playing), 2000)
eq(s.artworkStarted, null, 'Playing before card attachment does not spend the animation')
s = T.step(s, p(playing), 2500, {episode:1000,attached:true})
eq(s.artworkStarted, 2500, 'Attachment starts the Playing clock once')
eq(T.presentation(s, 2999, 1000).artworkProgress, 0, 'Initial pose holds for half a second')
eq(T.presentation(s, 3500, 1000).artworkProgress, 0.5, 'The pose comes from the same clock')
s = T.step(s, p(linking), 3600, attached)
s = T.step(s, p(playing), 3700, attached)
eq(s.artworkStarted, 2500, 'Phase recovery never replays the turn')
eq(T.presentation(s, 4000, 1000).artworkProgress, 1, 'The turn stops at the final pose')
s = T.step(s, p(playing), 4999, attached)
eq(s.visible, true, 'Final hold uses the animation clock despite later phase changes')
s = T.step(s, p(playing), 5000, attached)
eq(s.visible, false, 'Both playback settle and animation hold are complete')

s = T.step(T.initial(), p(linking), 1000)
s = T.step(s, p(playing), 2000, attached, 0)
eq(T.presentation(s, 2000, 0).artworkProgress, 1, 'Reduced motion immediately shows the final pose')
s = T.step(s, p(playing), 3499, attached, 0)
eq(s.visible, true, 'Reduced motion retains the existing completion display time')
s = T.step(s, p(playing), 3500, attached, 0)
eq(s.visible, false, 'Reduced motion finishes without waiting for animation frames')

s = T.step(T.initial(), p(linking), 1000)
s = T.step(s, p(linking), 2000, {episode:500,failed:true,attached:true,dismissed:true})
eq(s.visible, true, 'Stale publication feedback cannot close the current episode')
s = T.step(s, p(linking), 2100, {episode:1000,failed:true})
eq(s.visible, false, 'Current publication failure closes the toast')
s = T.step(s, p(linking), 2200)
eq(s.visible, false, 'A failed publication does not retry the same attempt')
console.log('Unified animation clock, reduced motion and publication feedback checks passed')

// Preview substitutes scripted input for live status and has one end path.
let shown = T.advance(p(idle), 1000, {}, 1000)
eq([shown.visible, shown.preview], [false, false], 'Idle status shows nothing before a preview')
T.preview(2000)
shown = T.advance(p(idle), 2000, {}, 1000)
eq([shown.visible, shown.phase, shown.preview], [true, 'connecting', true], 'A preview starts its own episode')
const previewEpisode = shown.episode
eq(T.advance(p(idle), 5000, {}, 1000).phase, 'preparing', 'The scripted phases follow the preview clock')
shown = T.advance(p(idle), 7500, {episode: previewEpisode, attached: true}, 1000)
eq([shown.phase, shown.visible], ['playing', true], 'Preview playback starts the artwork clock')
shown = T.advance(p(idle), 10000, {episode: previewEpisode, attached: true}, 1000)
eq([shown.visible, shown.preview], [false, true], 'The closing transition is still marked as a preview')
shown = T.advance(p(idle), 10250, {}, 1000)
eq([shown.visible, shown.preview, shown.episode], [false, false, 0], 'Live status resumes once the preview card closes')
T.preview(20000)
T.advance(p(idle), 20000, {}, 1000)
shown = T.advance(p(idle), 20000 + 14000, {}, 1000, true)
eq(T.advance(p(idle), 34250, {}, 1000).preview, false, 'An untracked preview still ends at its limit')

// Disabling is an input to the model, not a fabricated native dismissal.
s = T.step(T.initial(), p(idle), 1000, {}, 1000, false)
s = T.step(s, p(playing), 2000, {}, 1000, false)
eq(s.visible, false, 'A disabled toast stays hidden through a fast takeover')
s = T.step(s, p(playing), 3000, {}, 1000, true)
eq(s.visible, false, 'Enabling does not announce established playback')
s = T.step(T.initial(), p(idle), 1000)
s = T.step(s, p(linking), 2000)
s = T.step(s, p(linking), 3000, {}, 1000, false)
eq(s.visible, false, 'Disabling hides a visible attempt')
s = T.step(s, p(linking), 4000, {}, 1000, true)
eq(s.visible, true, 'Enabling during an attempt in flight shows it again')
eq(T.LIMIT_MS, 30000, 'The native expiry and the model share one limit')
console.log('Preview and disabled-setting checks passed')
