// macOS: the persisted FSEvents log — how far back it reaches and how fast it replays (2026-09-29).
// Build: cc -O2 -Wno-deprecated-declarations -framework CoreServices -o fsevents_probe fsevents_probe.c
// BACK=N replays the last N event ids; else HOURS_AGO through FSEventsGetLastEventIdForDeviceBeforeTime.
// How far back the volume's persisted FSEvents log reaches, and how fast it replays.
// usage: fsevents_probe PATH HOURS_AGO
#include <CoreServices/CoreServices.h>
#include <sys/stat.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
static unsigned long long lastid, firstid; static unsigned long events, dirs_must_scan, wrapped, history_done, root_changed; static double t0;
static char first[1024];
static void cb(ConstFSEventStreamRef s, void*ctx, size_t n, void*paths, const FSEventStreamEventFlags f[], const FSEventStreamEventId ids[]){
    char**p=paths;
    for(size_t i=0;i<n;i++){
        if(f[i]&kFSEventStreamEventFlagHistoryDone){ history_done=1; CFRunLoopStop(CFRunLoopGetCurrent()); continue; }

        if(events==0) snprintf(first,sizeof first,"%s",p[i]);
        if(events==0) firstid=ids[i]; lastid=ids[i]; events++;
        if(f[i]&kFSEventStreamEventFlagMustScanSubDirs) dirs_must_scan++;
        if(f[i]&kFSEventStreamEventFlagEventIdsWrapped) wrapped++;
        if(f[i]&kFSEventStreamEventFlagRootChanged) root_changed++;
    }
}
int main(int argc,char**argv){
    const char*path=argc>1?argv[1]:"/Users/angch"; double hours=argc>2?atof(argv[2]):24;
    struct stat st; stat(path,&st);
    CFAbsoluteTime when=CFAbsoluteTimeGetCurrent()-hours*3600;
    FSEventStreamEventId now=FSEventsGetCurrentEventId();
    FSEventStreamEventId since=getenv("BACK")?now-strtoull(getenv("BACK"),0,10):FSEventsGetLastEventIdForDeviceBeforeTime(st.st_dev,when);
    if(getenv("BACK")&&since>now) since=1;
    printf("device %d: event id %llu at %.1f h ago, %llu now (%llu events between)\n",(int)st.st_dev,(unsigned long long)since,hours,(unsigned long long)now,(unsigned long long)(now-since));
    if(since==0){ printf("no event id that far back: the log does not reach it\n"); return 0; }
    CFStringRef cfp=CFStringCreateWithCString(NULL,path,kCFStringEncodingUTF8);
    CFArrayRef paths=CFArrayCreate(NULL,(const void**)&cfp,1,&kCFTypeArrayCallBacks);
    FSEventStreamRef s=FSEventStreamCreate(NULL,cb,NULL,paths,since,0.0,kFSEventStreamCreateFlagNone);
    t0=CFAbsoluteTimeGetCurrent();
    FSEventStreamScheduleWithRunLoop(s,CFRunLoopGetCurrent(),kCFRunLoopDefaultMode);
    FSEventStreamStart(s);
    CFRunLoopRunInMode(kCFRunLoopDefaultMode,120,false);
    double dt=CFAbsoluteTimeGetCurrent()-t0;
    printf("ids %llu..%llu; replayed %lu directory events in %.2fs (history done: %lu, must-scan-subdirs: %lu, ids wrapped: %lu, root changed: %lu)\n  first: %s\n",firstid,lastid,events,dt,history_done,dirs_must_scan,wrapped,root_changed,first);
    return 0;
}
