#pragma once
#include "CoreMinimal.h"
#include "ILiveLinkSource.h"
#include "ILiveLinkClient.h"
#include "MetaHumanAudioBaseLiveLinkSubject.h"
#include "MetaHumanAudioBaseLiveLinkSubjectSettings.h"
#include "UObject/StrongObjectPtr.h"
class FEchoAudioSubject final : public FMetaHumanAudioBaseLiveLinkSubject {
public:
 FEchoAudioSubject(ILiveLinkClient* Client,FGuid Guid,UMetaHumanAudioBaseLiveLinkSubjectSettings* Settings)
 :FMetaHumanAudioBaseLiveLinkSubject(Client,Guid,TEXT("EchoSpeech"),Settings) {}
 void Start() override { FMetaHumanLocalLiveLinkSubject::Start(); }
 void Push(TArray<float> Samples,int32 Rate,uint64 StartSample) {
  FAudioSample Sample; Sample.NumChannels=1; Sample.SampleRate=Rate; Sample.NumSamples=Samples.Num();
  Sample.Data=MoveTemp(Samples); Sample.Time=FQualifiedFrameTime(FFrameTime((int32)StartSample),FFrameRate(Rate,1));
  Sample.TimeSource=ETimeSource::Media; AddAudioSample(MoveTemp(Sample));
 }
protected: void MediaSamplerMain() override {}
};
class FEchoAudioSource final : public ILiveLinkSource {
public:
 TStrongObjectPtr<UMetaHumanAudioBaseLiveLinkSubjectSettings> Settings;
 TUniquePtr<FEchoAudioSubject> Subject;
 ILiveLinkClient* Client=nullptr; FGuid Guid;
 void ReceiveClient(ILiveLinkClient* InClient,FGuid InGuid) override { Client=InClient; Guid=InGuid; }
 void Reset() {
  if(Subject) { Subject->Stop(); Subject.Reset(); }
  Settings.Reset(NewObject<UMetaHumanAudioBaseLiveLinkSubjectSettings>());
  Settings->Lookahead=80;
  if(Client) { Subject=MakeUnique<FEchoAudioSubject>(Client,Guid,Settings.Get()); Subject->Start(); }
 }
 bool IsSourceStillValid() const override { return Subject.IsValid(); }
 bool RequestSourceShutdown() override { if(Subject) Subject->Stop(); Subject.Reset(); return true; }
 FText GetSourceType() const override { return FText::FromString(TEXT("Echo streaming speech")); }
 FText GetSourceMachineName() const override { return FText::FromString(TEXT("Local")); }
 FText GetSourceStatus() const override { return FText::FromString(TEXT("Streaming")); }
};
